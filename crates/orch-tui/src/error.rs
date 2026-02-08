#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error("terminal io error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },
}
