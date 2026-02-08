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

impl GraphiteError {
    pub fn is_restack_conflict(&self) -> bool {
        match self {
            GraphiteError::CommandFailed { stdout, stderr, .. } => {
                looks_like_restack_conflict(stdout, stderr)
            }
            _ => false,
        }
    }
}

pub fn looks_like_restack_conflict(stdout: &str, stderr: &str) -> bool {
    let combined = format!("{}\n{}", stdout, stderr).to_ascii_lowercase();

    let markers = [
        "conflict",
        "merge conflict",
        "could not apply",
        "needs resolution",
        "resolve conflicts",
        "gt continue",
    ];

    markers.iter().any(|marker| combined.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::{looks_like_restack_conflict, GraphiteError};

    #[test]
    fn detects_restack_conflict_from_common_markers() {
        assert!(looks_like_restack_conflict(
            "",
            "CONFLICT (content): Merge conflict in src/main.rs"
        ));
        assert!(looks_like_restack_conflict(
            "could not apply 123abc",
            "please resolve conflicts and run gt continue"
        ));
    }

    #[test]
    fn does_not_flag_unrelated_failures_as_conflicts() {
        assert!(!looks_like_restack_conflict(
            "",
            "authentication failed: token expired"
        ));
    }

    #[test]
    fn graph_error_helper_identifies_only_conflict_failures() {
        let conflict = GraphiteError::CommandFailed {
            command: "gt restack".to_string(),
            status: Some(1),
            stdout: "".to_string(),
            stderr: "merge conflict".to_string(),
        };
        assert!(conflict.is_restack_conflict());

        let other = GraphiteError::ContractViolation {
            message: "bad args".to_string(),
        };
        assert!(!other.is_restack_conflict());
    }
}
