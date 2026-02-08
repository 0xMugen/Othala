use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::GitError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCli {
    pub binary: PathBuf,
}

impl Default for GitCli {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("git"),
        }
    }
}

impl GitCli {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
        }
    }

    pub fn run<I, S>(&self, cwd: &Path, args: I) -> Result<GitOutput, GitError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let owned_args: Vec<OsString> = args
            .into_iter()
            .map(|arg| arg.as_ref().to_os_string())
            .collect();

        let mut command = Command::new(&self.binary);
        command.current_dir(cwd);
        for arg in &owned_args {
            command.arg(arg);
        }

        let rendered = render_command(&self.binary, &owned_args);
        let output = command.output().map_err(|source| GitError::Io {
            command: rendered.clone(),
            source,
        })?;

        let stdout =
            String::from_utf8(output.stdout).map_err(|source| GitError::NonUtf8Output {
                command: rendered.clone(),
                stream: "stdout",
                source,
            })?;
        let stderr =
            String::from_utf8(output.stderr).map_err(|source| GitError::NonUtf8Output {
                command: rendered.clone(),
                stream: "stderr",
                source,
            })?;

        if !output.status.success() {
            return Err(GitError::CommandFailed {
                command: rendered,
                status: output.status.code(),
                stdout,
                stderr,
            });
        }

        Ok(GitOutput { stdout, stderr })
    }
}

fn render_command(binary: &Path, args: &[OsString]) -> String {
    let mut rendered = binary.to_string_lossy().into_owned();
    for arg in args {
        rendered.push(' ');
        rendered.push_str(&arg.to_string_lossy());
    }
    rendered
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::GitCli;
    use crate::error::GitError;

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("othala-orch-git-{prefix}-{now}"));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn run_returns_stdout_for_successful_command() {
        let git = GitCli::default();
        let cwd = unique_temp_dir("command-ok");

        let output = git
            .run(&cwd, ["--version"])
            .expect("git --version should succeed");

        assert!(output.stdout.to_ascii_lowercase().contains("git version"));
        let _ = fs::remove_dir_all(cwd);
    }

    #[test]
    fn run_classifies_non_zero_exit_as_command_failed() {
        let git = GitCli::default();
        let cwd = unique_temp_dir("command-fail");

        let err = git
            .run(&cwd, ["definitely-not-a-real-git-subcommand"])
            .expect_err("unknown git subcommand should fail");
        match err {
            GitError::CommandFailed {
                command,
                status,
                stdout: _,
                stderr,
            } => {
                assert!(command.contains("definitely-not-a-real-git-subcommand"));
                assert!(status.is_some());
                assert!(!stderr.trim().is_empty());
            }
            other => panic!("expected CommandFailed, got {other:?}"),
        }

        let _ = fs::remove_dir_all(cwd);
    }

    #[test]
    fn run_classifies_missing_binary_as_io_error() {
        let git = GitCli::new("/definitely/missing/git-binary");
        let cwd = unique_temp_dir("command-io");

        let err = git
            .run(&cwd, ["status"])
            .expect_err("missing binary should fail");
        match err {
            GitError::Io { command, source } => {
                assert!(command.contains("/definitely/missing/git-binary"));
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("expected Io, got {other:?}"),
        }

        let _ = fs::remove_dir_all(cwd);
    }
}
