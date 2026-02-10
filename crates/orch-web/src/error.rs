//! Web error types - MVP stub.

#[derive(Debug, thiserror::Error)]
pub enum WebError {
    #[error("not implemented in MVP")]
    NotImplemented,
}
