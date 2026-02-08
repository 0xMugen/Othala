use std::string::FromUtf8Error;

#[derive(Debug, thiserror::Error)]
pub enum GraphiteError {
    #[error("graphite command failed to start ({command}): {source}")]
    Io {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("graphite command returned non-zero exit ({command}) status={status:?}")]
    CommandFailed {
        command: String,
        status: Option<i32>,
        stdout: String,
        stderr: String,
    },
    #[error("graphite command output was not valid UTF-8 ({command}, {stream}): {source}")]
    NonUtf8Output {
        command: String,
        stream: &'static str,
        #[source]
        source: FromUtf8Error,
    },
    #[error("graphite contract violation: {message}")]
    ContractViolation { message: String },
    #[error("unable to parse graphite output: {message}")]
    Parse { message: String },
}
