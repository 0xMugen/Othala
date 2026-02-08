#[derive(Debug, thiserror::Error)]
pub enum NotifyError {
    #[error("notification sink is disabled: {sink}")]
    SinkDisabled { sink: String },
    #[error("notification sink failed: {message}")]
    SinkFailed { message: String },
}

#[cfg(test)]
mod tests {
    use super::NotifyError;

    #[test]
    fn sink_disabled_formats_sink_name() {
        let err = NotifyError::SinkDisabled {
            sink: "telegram".to_string(),
        };

        assert_eq!(err.to_string(), "notification sink is disabled: telegram");
        assert!(matches!(err, NotifyError::SinkDisabled { ref sink } if sink == "telegram"));
    }

    #[test]
    fn sink_failed_formats_failure_message() {
        let err = NotifyError::SinkFailed {
            message: "network timeout".to_string(),
        };

        assert_eq!(err.to_string(), "notification sink failed: network timeout");
        assert!(
            matches!(err, NotifyError::SinkFailed { ref message } if message == "network timeout")
        );
    }
}
