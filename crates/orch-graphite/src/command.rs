use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::GraphiteError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AllowedAutoCommand {
    Create,
    Restack,
    AddAllForConflict,
    ContinueConflict,
    LogShort,
    Status,
    Submit,
    SubmitStack,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphiteOutput {
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphiteCli {
    pub binary: PathBuf,
}

impl Default for GraphiteCli {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("gt"),
        }
    }
}

impl GraphiteCli {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
        }
    }

    pub fn run_allowed<I, S>(
        &self,
        cwd: &Path,
        allowed: AllowedAutoCommand,
        args: I,
    ) -> Result<GraphiteOutput, GraphiteError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let owned_args: Vec<OsString> = args
            .into_iter()
            .map(|arg| arg.as_ref().to_os_string())
            .collect();
        validate_contract(allowed, &owned_args)?;

        let mut command = Command::new(&self.binary);
        command.current_dir(cwd);
        for arg in &owned_args {
            command.arg(arg);
        }

        let rendered = render_command(&self.binary, &owned_args);
        let output = command.output().map_err(|source| GraphiteError::Io {
            command: rendered.clone(),
            source,
        })?;

        let stdout =
            String::from_utf8(output.stdout).map_err(|source| GraphiteError::NonUtf8Output {
                command: rendered.clone(),
                stream: "stdout",
                source,
            })?;
        let stderr =
            String::from_utf8(output.stderr).map_err(|source| GraphiteError::NonUtf8Output {
                command: rendered.clone(),
                stream: "stderr",
                source,
            })?;

        if !output.status.success() {
            return Err(GraphiteError::CommandFailed {
                command: rendered,
                status: output.status.code(),
                stdout,
                stderr,
            });
        }

        Ok(GraphiteOutput { stdout, stderr })
    }
}

fn validate_contract(
    allowed: AllowedAutoCommand,
    args: &[OsString],
) -> Result<(), GraphiteError> {
    let ok = match allowed {
        AllowedAutoCommand::Create => {
            args.len() == 2 && arg_eq(args, 0, "create") && !arg_at(args, 1).trim().is_empty()
        }
        AllowedAutoCommand::Restack => args.len() == 1 && arg_eq(args, 0, "restack"),
        AllowedAutoCommand::AddAllForConflict => {
            args.len() == 2 && arg_eq(args, 0, "add") && arg_eq(args, 1, "-A")
        }
        AllowedAutoCommand::ContinueConflict => args.len() == 1 && arg_eq(args, 0, "continue"),
        AllowedAutoCommand::LogShort => {
            args.len() == 2 && arg_eq(args, 0, "log") && arg_eq(args, 1, "short")
        }
        AllowedAutoCommand::Status => args.len() == 1 && arg_eq(args, 0, "status"),
        AllowedAutoCommand::Submit => args.len() == 1 && arg_eq(args, 0, "submit"),
        AllowedAutoCommand::SubmitStack => {
            args.len() == 2 && arg_eq(args, 0, "submit") && arg_eq(args, 1, "--stack")
        }
    };

    if ok {
        return Ok(());
    }

    Err(GraphiteError::ContractViolation {
        message: format!("disallowed automated graphite invocation: {:?}", args),
    })
}

fn arg_eq(args: &[OsString], idx: usize, expected: &str) -> bool {
    arg_at(args, idx) == expected
}

fn arg_at(args: &[OsString], idx: usize) -> String {
    args.get(idx)
        .map(|x| x.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn render_command(binary: &Path, args: &[OsString]) -> String {
    let mut rendered = binary.to_string_lossy().into_owned();
    for arg in args {
        rendered.push(' ');
        rendered.push_str(&arg.to_string_lossy());
    }
    rendered
}
