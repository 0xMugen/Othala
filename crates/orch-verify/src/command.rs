use std::path::Path;
use std::process::Command;

use crate::error::VerifyError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCommandOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

pub fn run_shell_command(
    cwd: &Path,
    shell_bin: &str,
    command_line: &str,
) -> Result<ShellCommandOutput, VerifyError> {
    let rendered = format!("{shell_bin} -lc {command_line}");
    let output = Command::new(shell_bin)
        .arg("-lc")
        .arg(command_line)
        .current_dir(cwd)
        .output()
        .map_err(|source| VerifyError::Io {
            command: rendered.clone(),
            source,
        })?;

    let stdout = String::from_utf8(output.stdout).map_err(|source| VerifyError::NonUtf8Output {
        command: rendered.clone(),
        stream: "stdout",
        source,
    })?;
    let stderr = String::from_utf8(output.stderr).map_err(|source| VerifyError::NonUtf8Output {
        command: rendered,
        stream: "stderr",
        source,
    })?;

    Ok(ShellCommandOutput {
        exit_code: output.status.code(),
        success: output.status.success(),
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::run_shell_command;
    use crate::error::VerifyError;

    #[test]
    fn run_shell_command_returns_stdout_for_successful_command() {
        let cwd = std::env::current_dir().expect("resolve cwd");
        let output = run_shell_command(cwd.as_path(), "bash", "printf 'ok'")
            .expect("successful shell command");

        assert!(output.success);
        assert_eq!(output.exit_code, Some(0));
        assert_eq!(output.stdout, "ok");
        assert_eq!(output.stderr, "");
    }

    #[test]
    fn run_shell_command_returns_non_zero_exit_and_stderr() {
        let cwd = std::env::current_dir().expect("resolve cwd");
        let output = run_shell_command(cwd.as_path(), "bash", "printf 'bad' 1>&2; exit 7")
            .expect("command should still return output structure");

        assert!(!output.success);
        assert_eq!(output.exit_code, Some(7));
        assert!(output.stderr.ends_with("bad"));
    }

    #[test]
    fn run_shell_command_classifies_missing_shell_as_io_error() {
        let cwd = std::env::current_dir().expect("resolve cwd");
        let err = run_shell_command(
            cwd.as_path(),
            "definitely-not-a-shell-binary-for-othala-tests",
            "echo ignored",
        )
        .expect_err("missing shell should fail");

        assert!(
            matches!(err, VerifyError::Io { ref command, .. } if command.contains("definitely-not-a-shell-binary-for-othala-tests -lc echo ignored"))
        );
    }
}
