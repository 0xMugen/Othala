//! MVP verify runner - runs a single verification command.

use std::path::Path;
use std::process::Command;
use std::time::Instant;

use orch_core::config::RepoConfig;

use crate::error::VerifyError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyResult {
    pub success: bool,
    pub command: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiVerifyResult {
    pub overall_success: bool,
    pub results: Vec<VerifyResult>,
    pub total_duration_ms: u64,
}

/// Run the verification command for a repo.
pub fn run_verify(
    repo_config: &RepoConfig,
    worktree_path: &Path,
) -> Result<VerifyResult, VerifyError> {
    let command = &repo_config.verify.command;

    if command.trim().is_empty() {
        return Ok(VerifyResult {
            success: true,
            command: "(no verify command configured)".to_string(),
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(0),
            duration_ms: 0,
        });
    }

    let start = Instant::now();
    let output = Command::new("bash")
        .arg("-lc")
        .arg(command)
        .current_dir(worktree_path)
        .output()
        .map_err(|source| VerifyError::Io {
            command: command.clone(),
            source,
        })?;
    let duration_ms = start.elapsed().as_millis() as u64;

    let stdout = String::from_utf8(output.stdout).map_err(|source| VerifyError::NonUtf8Output {
        command: command.clone(),
        stream: "stdout",
        source,
    })?;

    let stderr = String::from_utf8(output.stderr).map_err(|source| VerifyError::NonUtf8Output {
        command: command.clone(),
        stream: "stderr",
        source,
    })?;

    Ok(VerifyResult {
        success: output.status.success(),
        command: command.clone(),
        stdout,
        stderr,
        exit_code: output.status.code(),
        duration_ms,
    })
}

pub fn run_multi_verify(
    commands: &[String],
    worktree_path: &Path,
) -> Result<MultiVerifyResult, VerifyError> {
    let total_start = Instant::now();
    let mut results = Vec::new();
    let mut overall_success = true;

    for cmd in commands {
        let config = RepoConfig {
            repo_id: String::new(),
            repo_path: worktree_path.to_path_buf(),
            base_branch: String::new(),
            nix: orch_core::config::NixConfig {
                dev_shell: String::new(),
            },
            verify: orch_core::config::VerifyConfig {
                command: cmd.clone(),
            },
            graphite: orch_core::config::RepoGraphiteConfig {
                draft_on_start: false,
                submit_mode: None,
            },
        };

        let result = run_verify(&config, worktree_path)?;
        if !result.success {
            overall_success = false;
            results.push(result);
            break;
        }
        results.push(result);
    }

    Ok(MultiVerifyResult {
        overall_success,
        results,
        total_duration_ms: total_start.elapsed().as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::config::{NixConfig, RepoGraphiteConfig, VerifyConfig};
    use orch_core::types::SubmitMode;
    use std::path::PathBuf;

    fn mk_repo_config(verify_command: &str) -> RepoConfig {
        RepoConfig {
            repo_id: "test".to_string(),
            repo_path: PathBuf::from("/tmp/test"),
            base_branch: "main".to_string(),
            nix: NixConfig {
                dev_shell: "nix develop".to_string(),
            },
            verify: VerifyConfig {
                command: verify_command.to_string(),
            },
            graphite: RepoGraphiteConfig {
                draft_on_start: false,
                submit_mode: Some(SubmitMode::Single),
            },
        }
    }

    #[test]
    fn run_verify_succeeds_with_true() {
        let config = mk_repo_config("true");
        let result = run_verify(&config, Path::new("/tmp")).expect("run verify");
        assert!(result.success);
        assert_eq!(result.exit_code, Some(0));
    }

    #[test]
    fn run_verify_fails_with_false() {
        let config = mk_repo_config("false");
        let result = run_verify(&config, Path::new("/tmp")).expect("run verify");
        assert!(!result.success);
        assert_eq!(result.exit_code, Some(1));
    }

    #[test]
    fn run_verify_captures_output() {
        let config = mk_repo_config("echo hello && echo world >&2");
        let result = run_verify(&config, Path::new("/tmp")).expect("run verify");
        assert!(result.success);
        assert!(result.stdout.contains("hello"));
        assert!(result.stderr.contains("world"));
    }

    #[test]
    fn run_verify_passes_with_empty_command() {
        let config = mk_repo_config("");
        let result = run_verify(&config, Path::new("/tmp")).expect("run verify");
        assert!(result.success);
        assert_eq!(result.duration_ms, 0);
    }

    #[test]
    fn run_verify_includes_duration() {
        let config = mk_repo_config("true");
        let result = run_verify(&config, Path::new("/tmp")).expect("run verify");
        assert!(result.success);
        assert!(result.duration_ms < 5_000);
    }

    #[test]
    fn multi_verify_runs_all_on_success() {
        let commands = vec!["true".to_string(), "echo ok".to_string()];
        let result =
            super::run_multi_verify(&commands, Path::new("/tmp")).expect("run multi verify");
        assert!(result.overall_success);
        assert_eq!(result.results.len(), 2);
        assert!(result.results[0].success);
        assert!(result.results[1].success);
    }

    #[test]
    fn multi_verify_stops_on_first_failure() {
        let commands = vec![
            "true".to_string(),
            "false".to_string(),
            "echo should_not_run".to_string(),
        ];
        let result =
            super::run_multi_verify(&commands, Path::new("/tmp")).expect("run multi verify");
        assert!(!result.overall_success);
        assert_eq!(result.results.len(), 2);
        assert!(result.results[0].success);
        assert!(!result.results[1].success);
    }

    #[test]
    fn multi_verify_empty_commands_succeeds() {
        let commands: Vec<String> = vec![];
        let result =
            super::run_multi_verify(&commands, Path::new("/tmp")).expect("run multi verify");
        assert!(result.overall_success);
        assert!(result.results.is_empty());
    }
}
