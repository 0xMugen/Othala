#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error("terminal io error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::TuiError;
    use std::error::Error;

    #[test]
    fn io_error_converts_via_from_and_preserves_source() {
        let source = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "tty closed");
        let err: TuiError = source.into();

        let rendered = err.to_string();
        assert!(rendered.contains("terminal io error"));
        assert!(rendered.contains("tty closed"));
        assert!(err.source().is_some());
    }
}
