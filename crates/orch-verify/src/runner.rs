//! MVP verify runner - runs a single verification command.

use std::path::Path;
use std::process::Command;

use orch_core::config::RepoConfig;

use crate::error::VerifyError;

/// Result of running a verification command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyResult {
    pub success: bool,
    pub command: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

/// Run the verification command for a repo.
pub fn run_verify(
    repo_config: &RepoConfig,
    worktree_path: &Path,
) -> Result<VerifyResult, VerifyError> {
    let command = &repo_config.verify.command;

    if command.trim().is_empty() {
        // No verify command configured - consider it a pass
        return Ok(VerifyResult {
            success: true,
            command: "(no verify command configured)".to_string(),
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(0),
        });
    }

    let output = Command::new("bash")
        .arg("-lc")
        .arg(command)
        .current_dir(worktree_path)
        .output()
        .map_err(|source| VerifyError::Io {
            command: command.clone(),
            source,
        })?;

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
    }
}
