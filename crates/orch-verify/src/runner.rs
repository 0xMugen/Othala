use chrono::Utc;
use orch_core::config::RepoConfig;
use orch_core::state::VerifyTier;
use std::path::Path;

use crate::command::run_shell_command;
use crate::discover::resolve_verify_commands;
use crate::error::VerifyError;
use crate::types::{
    PreparedVerifyCommand, VerifyCommandResult, VerifyFailureClass, VerifyOutcome, VerifyResult,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyRunner {
    shell_bin: String,
    fail_fast: bool,
}

impl Default for VerifyRunner {
    fn default() -> Self {
        Self {
            shell_bin: "bash".to_string(),
            fail_fast: true,
        }
    }
}

impl VerifyRunner {
    pub fn new(shell_bin: impl Into<String>) -> Self {
        Self {
            shell_bin: shell_bin.into(),
            fail_fast: true,
        }
    }

    pub fn with_fail_fast(mut self, fail_fast: bool) -> Self {
        self.fail_fast = fail_fast;
        self
    }

    pub fn run_tier_from_repo_config(
        &self,
        repo_config: &RepoConfig,
        tier: VerifyTier,
    ) -> Result<VerifyResult, VerifyError> {
        let configured = commands_for_tier(repo_config, tier);
        let commands = resolve_verify_commands(&repo_config.repo_path, tier, &configured);
        self.run_tier(
            &repo_config.repo_path,
            &repo_config.nix.dev_shell,
            tier,
            commands.as_slice(),
        )
    }

    pub fn run_tier(
        &self,
        repo_path: &Path,
        dev_shell_prefix: &str,
        tier: VerifyTier,
        commands: &[String],
    ) -> Result<VerifyResult, VerifyError> {
        if commands.is_empty() {
            return Err(VerifyError::InvalidConfig {
                message: format!("verify.{tier:?}.commands must contain at least one command"),
            });
        }
        if dev_shell_prefix.trim().is_empty() {
            return Err(VerifyError::InvalidConfig {
                message: "nix.dev_shell must not be empty".to_string(),
            });
        }

        let started_at = Utc::now();
        let mut command_results = Vec::with_capacity(commands.len());

        for raw_command in commands {
            let prepared = prepare_verify_command(dev_shell_prefix, raw_command);
            let command_started_at = Utc::now();
            let output = run_shell_command(repo_path, &self.shell_bin, &prepared.effective)?;
            let command_finished_at = Utc::now();

            let failure_class = if output.success {
                None
            } else {
                Some(classify_failure(&output.stdout, &output.stderr))
            };
            let outcome = if output.success {
                VerifyOutcome::Passed
            } else {
                VerifyOutcome::Failed
            };

            command_results.push(VerifyCommandResult {
                command: prepared,
                outcome,
                failure_class,
                exit_code: output.exit_code,
                started_at: command_started_at,
                finished_at: command_finished_at,
                stdout: output.stdout,
                stderr: output.stderr,
            });

            if !output.success && self.fail_fast {
                break;
            }
        }

        let overall = if command_results
            .iter()
            .all(|result| result.outcome == VerifyOutcome::Passed)
        {
            VerifyOutcome::Passed
        } else {
            VerifyOutcome::Failed
        };

        Ok(VerifyResult {
            tier,
            outcome: overall,
            started_at,
            finished_at: Utc::now(),
            commands: command_results,
        })
    }
}

pub fn commands_for_tier(repo_config: &RepoConfig, tier: VerifyTier) -> Vec<String> {
    match tier {
        VerifyTier::Quick => repo_config.verify.quick.commands.clone(),
        VerifyTier::Full => repo_config.verify.full.commands.clone(),
    }
}

pub fn prepare_verify_command(dev_shell_prefix: &str, raw_command: &str) -> PreparedVerifyCommand {
    let original = raw_command.trim().to_string();
    let normalized_dev_shell = normalize_spaces(dev_shell_prefix);
    let normalized_original = normalize_spaces(&original);

    let already_wrapped = command_has_prefix(&normalized_original, &normalized_dev_shell)
        || command_has_prefix(&normalized_original, "nix develop")
        || command_has_prefix(&normalized_original, "nix shell")
        || command_has_prefix(&normalized_original, "nix-shell");

    if already_wrapped {
        return PreparedVerifyCommand {
            original,
            effective: raw_command.trim().to_string(),
            wrapped_with_dev_shell: false,
        };
    }

    let escaped = shell_quote_single(&original);
    PreparedVerifyCommand {
        original,
        effective: format!("{} -c {}", dev_shell_prefix.trim(), escaped),
        wrapped_with_dev_shell: true,
    }
}

fn normalize_spaces(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn command_has_prefix(command: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return false;
    }
    if command == prefix {
        return true;
    }
    command.starts_with(prefix) && command[prefix.len()..].starts_with(' ')
}

fn shell_quote_single(value: &str) -> String {
    let escaped = value.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

fn classify_failure(stdout: &str, stderr: &str) -> VerifyFailureClass {
    let combined = format!("{}\n{}", stdout, stderr).to_ascii_lowercase();

    if combined.contains("test failed")
        || combined.contains("failing tests")
        || combined.contains("assertion failed")
    {
        return VerifyFailureClass::Tests;
    }
    if combined.contains("clippy")
        || combined.contains("lint")
        || combined.contains("denied warning")
    {
        return VerifyFailureClass::Lint;
    }
    if combined.contains("rustfmt") || combined.contains("format") {
        return VerifyFailureClass::Format;
    }
    if combined.contains("could not resolve")
        || combined.contains("failed to fetch")
        || combined.contains("permission denied")
        || combined.contains("not found")
        || combined.contains("unable to")
        || combined.contains("network")
    {
        return VerifyFailureClass::Environment;
    }
    if combined.contains("error:") || combined.contains("linker") || combined.contains("compile")
    {
        return VerifyFailureClass::Build;
    }

    VerifyFailureClass::Unknown
}
