use orch_core::types::ModelKind;

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("no adapter available for model {model:?}")]
    UnsupportedModel { model: ModelKind },
    #[error("invalid epoch request: {message}")]
    InvalidRequest { message: String },
    #[error("pty setup failed: {message}")]
    PtySetup { message: String },
    #[error("agent spawn failed: {message}")]
    Spawn { message: String },
    #[error("agent runtime error: {message}")]
    Runtime { message: String },
}

#[cfg(test)]
mod tests {
    use orch_core::types::ModelKind;

    use super::AgentError;

    #[test]
    fn unsupported_model_error_includes_model_name() {
        let err = AgentError::UnsupportedModel {
            model: ModelKind::Gemini,
        };
        let text = err.to_string();
        assert!(text.contains("no adapter available for model"));
        assert!(text.contains("Gemini"));
    }

    #[test]
    fn invalid_request_error_formats_message() {
        let err = AgentError::InvalidRequest {
            message: "prompt must not be empty".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "invalid epoch request: prompt must not be empty"
        );
    }

    #[test]
    fn pty_setup_error_formats_message() {
        let err = AgentError::PtySetup {
            message: "failed to open pty".to_string(),
        };
        assert_eq!(err.to_string(), "pty setup failed: failed to open pty");
    }

    #[test]
    fn spawn_error_formats_message() {
        let err = AgentError::Spawn {
            message: "command not found".to_string(),
        };
        assert_eq!(err.to_string(), "agent spawn failed: command not found");
    }

    #[test]
    fn runtime_error_formats_message() {
        let err = AgentError::Runtime {
            message: "waitpid failed".to_string(),
        };
        assert_eq!(err.to_string(), "agent runtime error: waitpid failed");
    }
}
