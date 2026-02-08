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
