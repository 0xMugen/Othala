use std::string::FromUtf8Error;

#[derive(Debug, thiserror::Error)]
pub enum GraphiteError {
    #[error("graphite command failed to start ({command}): {source}")]
    Io {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{}", format_command_failed(.command, .status, .stdout, .stderr))]
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

fn format_command_failed(
    command: &str,
    status: &Option<i32>,
    stdout: &str,
    stderr: &str,
) -> String {
    let base = format!("graphite command returned non-zero exit ({command}) status={status:?}");
    let combined = [stderr.trim(), stdout.trim()]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" | ");
    if combined.is_empty() {
        base
    } else {
        let snippet: String = combined.chars().take(400).collect();
        format!("{base}: {snippet}")
    }
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

    pub fn is_auth_failure(&self) -> bool {
        match self {
            GraphiteError::CommandFailed { stdout, stderr, .. } => {
                looks_like_auth_failure(stdout, stderr)
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

pub fn looks_like_auth_failure(stdout: &str, stderr: &str) -> bool {
    let combined = format!("{}\n{}", stdout, stderr).to_ascii_lowercase();

    let markers = [
        "please authenticate your graphite cli",
        "no auth token set",
        "graphite auth --token",
        "graphite.com/activate",
        "authentication failed",
        "not authenticated",
    ];

    markers.iter().any(|marker| combined.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::{looks_like_auth_failure, looks_like_restack_conflict, GraphiteError};

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

    #[test]
    fn detects_graphite_auth_failures() {
        assert!(looks_like_auth_failure(
            "",
            "ERROR: Please authenticate your Graphite CLI by visiting https://app.graphite.com/activate"
        ));
        assert!(looks_like_auth_failure(
            "",
            "ERROR: No auth token set. Please run `graphite auth --token <token>`."
        ));

        let err = GraphiteError::CommandFailed {
            command: "gt submit --no-edit --no-interactive".to_string(),
            status: Some(1),
            stdout: "".to_string(),
            stderr: "ERROR: No auth token set".to_string(),
        };
        assert!(err.is_auth_failure());
    }
}
