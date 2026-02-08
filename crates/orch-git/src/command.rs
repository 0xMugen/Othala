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
