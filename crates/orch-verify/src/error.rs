use std::string::FromUtf8Error;

#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("invalid verify configuration: {message}")]
    InvalidConfig { message: String },
    #[error("verify command failed to start ({command}): {source}")]
    Io {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("verify command output was not valid UTF-8 ({command}, {stream}): {source}")]
    NonUtf8Output {
        command: String,
        stream: &'static str,
        #[source]
        source: FromUtf8Error,
    },
}
