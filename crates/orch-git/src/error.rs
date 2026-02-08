use std::path::PathBuf;
use std::string::FromUtf8Error;

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("git command failed to start ({command}): {source}")]
    Io {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("git command returned non-zero exit ({command}) status={status:?}")]
    CommandFailed {
        command: String,
        status: Option<i32>,
        stdout: String,
        stderr: String,
    },
    #[error("git command output was not valid UTF-8 ({command}, {stream}): {source}")]
    NonUtf8Output {
        command: String,
        stream: &'static str,
        #[source]
        source: FromUtf8Error,
    },
    #[error("path is not inside a git repository: {path}")]
    NotARepository { path: PathBuf },
    #[error("invalid git output: {context}")]
    Parse { context: String },
}
