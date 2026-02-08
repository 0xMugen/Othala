#[derive(Debug, thiserror::Error)]
pub enum NotifyError {
    #[error("notification sink is disabled: {sink}")]
    SinkDisabled { sink: String },
    #[error("notification sink failed: {message}")]
    SinkFailed { message: String },
}
