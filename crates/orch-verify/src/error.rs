use std::string::FromUtf8Error;

#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("invalid verify configuration: {message}")]
    InvalidConfig { message: String },
    #[error("verify command failed to start ({command}): {source}")]
    Io {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("verify command output was not valid UTF-8 ({command}, {stream}): {source}")]
    NonUtf8Output {
        command: String,
        stream: &'static str,
        #[source]
        source: FromUtf8Error,
    },
}

#[cfg(test)]
mod tests {
    use super::VerifyError;
    use std::error::Error;

    #[test]
    fn invalid_config_variant_renders_message() {
        let err = VerifyError::InvalidConfig {
            message: "verify.quick.commands must not be empty".to_string(),
        };

        assert!(err
            .to_string()
            .contains("invalid verify configuration: verify.quick.commands must not be empty"));
        assert!(err.source().is_none());
    }

    #[test]
    fn io_variant_includes_command_and_preserves_source() {
        let err = VerifyError::Io {
            command: "bash -lc nix develop -c cargo test".to_string(),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        };

        let rendered = err.to_string();
        assert!(rendered.contains("verify command failed to start"));
        assert!(rendered.contains("(bash -lc nix develop -c cargo test)"));
        assert!(rendered.contains("denied"));
        assert!(err.source().is_some());
    }

    #[test]
    fn non_utf8_variant_mentions_stream_and_has_source() {
        let utf8_err = String::from_utf8(vec![0x80]).expect_err("invalid utf-8");
        let err = VerifyError::NonUtf8Output {
            command: "bash -lc echo".to_string(),
            stream: "stderr",
            source: utf8_err,
        };

        let rendered = err.to_string();
        assert!(rendered.contains("verify command output was not valid UTF-8"));
        assert!(rendered.contains("(bash -lc echo, stderr)"));
        assert!(err.source().is_some());
    }
}
