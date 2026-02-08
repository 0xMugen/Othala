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

#[cfg(test)]
mod tests {
    use super::GitError;
    use std::error::Error;
    use std::path::PathBuf;

    #[test]
    fn io_variant_includes_command_and_io_message() {
        let err = GitError::Io {
            command: "git status".to_string(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "missing binary"),
        };

        let rendered = err.to_string();
        assert!(rendered.contains("git command failed to start (git status)"));
        assert!(rendered.contains("missing binary"));
        assert!(err.source().is_some());
    }

    #[test]
    fn command_failed_variant_mentions_command_and_status() {
        let err = GitError::CommandFailed {
            command: "git rev-parse HEAD".to_string(),
            status: Some(128),
            stdout: String::new(),
            stderr: "fatal".to_string(),
        };

        let rendered = err.to_string();
        assert!(rendered.contains("git command returned non-zero exit (git rev-parse HEAD)"));
        assert!(rendered.contains("status=Some(128)"));
    }

    #[test]
    fn non_utf8_variant_mentions_stream_and_has_source() {
        let bytes = vec![0x80];
        let utf8_err = String::from_utf8(bytes).expect_err("invalid utf-8 bytes");
        let err = GitError::NonUtf8Output {
            command: "git status".to_string(),
            stream: "stdout",
            source: utf8_err,
        };

        let rendered = err.to_string();
        assert!(rendered.contains("git command output was not valid UTF-8"));
        assert!(rendered.contains("(git status, stdout)"));
        assert!(err.source().is_some());
    }

    #[test]
    fn repository_and_parse_variants_include_context() {
        let repo_err = GitError::NotARepository {
            path: PathBuf::from("/tmp/example"),
        };
        assert!(repo_err
            .to_string()
            .contains("path is not inside a git repository: /tmp/example"));

        let parse_err = GitError::Parse {
            context: "expected branch line".to_string(),
        };
        assert!(parse_err
            .to_string()
            .contains("invalid git output: expected branch line"));
    }
}
