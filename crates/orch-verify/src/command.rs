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
