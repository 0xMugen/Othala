use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
use std::fs;
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
    Markdown,
    StreamText,
}

impl fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text => write!(f, "text"),
            Self::Json => write!(f, "json"),
            Self::Markdown => write!(f, "markdown"),
            Self::StreamText => write!(f, "stream-text"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptRequest {
    pub prompt: String,
    pub model: Option<String>,
    pub format: OutputFormat,
    pub quiet: bool,
    pub system_prompt: Option<String>,
    pub max_tokens: Option<u64>,
    pub context_files: Vec<String>,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptResponse {
    pub content: String,
    pub model_used: String,
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub duration_ms: u64,
    pub session_id: String,
}

impl PromptResponse {
    pub fn format_output(&self, format: &OutputFormat) -> String {
        match format {
            OutputFormat::Text => self.content.clone(),
            OutputFormat::Markdown => {
                format!("## Response\n\n{}", self.content)
            }
            OutputFormat::StreamText => self
                .content
                .split_whitespace()
                .collect::<Vec<_>>()
                .join("\n"),
            OutputFormat::Json => {
                let usage = TokenUsage {
                    input: self.tokens_input,
                    output: self.tokens_output,
                    total: self.tokens_input + self.tokens_output,
                };

                let payload = JsonOutput {
                    content: self.content.clone(),
                    model: self.model_used.clone(),
                    tokens: usage,
                    duration_ms: self.duration_ms,
                    session_id: self.session_id.clone(),
                    timestamp: Utc::now().to_rfc3339(),
                };

                serde_json::to_string_pretty(&payload)
                    .unwrap_or_else(|err| format!("{{\"error\":\"{}\"}}", err))
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JsonOutput {
    pub content: String,
    pub model: String,
    pub tokens: TokenUsage,
    pub duration_ms: u64,
    pub session_id: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub total: u64,
}

pub struct PromptRunner {
    available_models: HashSet<String>,
    known_sessions: HashSet<String>,
    default_model: String,
}

impl Default for PromptRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl PromptRunner {
    pub fn new() -> Self {
        let models = ["claude", "codex", "gemini"]
            .into_iter()
            .map(ToString::to_string)
            .collect::<HashSet<_>>();

        Self {
            available_models: models,
            known_sessions: HashSet::new(),
            default_model: "codex".to_string(),
        }
    }

    pub fn validate_request(&self, request: &PromptRequest) -> Result<(), PromptError> {
        if request.prompt.trim().is_empty() {
            return Err(PromptError::EmptyPrompt);
        }

        if let Some(model) = &request.model {
            if !self.available_models.contains(model) {
                return Err(PromptError::ModelNotAvailable(model.clone()));
            }
        }

        for path in &request.context_files {
            if fs::metadata(path).is_err() {
                return Err(PromptError::ContextFileNotFound(path.clone()));
            }
        }

        if let Some(session_id) = &request.session_id {
            if !self.known_sessions.contains(session_id) {
                return Err(PromptError::SessionNotFound(session_id.clone()));
            }
        }

        Ok(())
    }

    pub fn format_response(
        &self,
        response: &PromptResponse,
        format: &OutputFormat,
    ) -> Result<String, PromptError> {
        let rendered = response.format_output(format);
        if rendered.is_empty() {
            return Err(PromptError::FormatError("rendered output is empty".to_string()));
        }
        Ok(rendered)
    }

    pub fn build_context(&self, files: &[String]) -> Result<String, PromptError> {
        let mut context = String::new();

        for path in files {
            let content = fs::read_to_string(path)
                .map_err(|_| PromptError::ContextFileNotFound(path.clone()))?;

            context.push_str("---\n");
            context.push_str(path);
            context.push('\n');
            context.push_str(&content);
            context.push('\n');
        }

        Ok(context)
    }

    pub fn register_session(&mut self, session_id: impl Into<String>) {
        self.known_sessions.insert(session_id.into());
    }

    pub fn resolve_model(&self, requested: Option<&str>) -> String {
        requested
            .map(ToString::to_string)
            .unwrap_or_else(|| self.default_model.clone())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PromptError {
    #[error("prompt cannot be empty")]
    EmptyPrompt,
    #[error("model not available: {0}")]
    ModelNotAvailable(String),
    #[error("context file not found: {0}")]
    ContextFileNotFound(String),
    #[error("failed to format output: {0}")]
    FormatError(String),
    #[error("session not found: {0}")]
    SessionNotFound(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_response() -> PromptResponse {
        PromptResponse {
            content: "hello world from model".to_string(),
            model_used: "codex".to_string(),
            tokens_input: 10,
            tokens_output: 15,
            duration_ms: 321,
            session_id: "sess-1".to_string(),
        }
    }

    fn valid_request() -> PromptRequest {
        PromptRequest {
            prompt: "Explain resource registry".to_string(),
            model: Some("codex".to_string()),
            format: OutputFormat::Text,
            quiet: false,
            system_prompt: Some("Be concise".to_string()),
            max_tokens: Some(200),
            context_files: vec![],
            session_id: None,
        }
    }

    #[test]
    fn output_format_default_is_text() {
        assert_eq!(OutputFormat::default(), OutputFormat::Text);
    }

    #[test]
    fn output_format_display_is_stable() {
        assert_eq!(OutputFormat::Text.to_string(), "text");
        assert_eq!(OutputFormat::Json.to_string(), "json");
        assert_eq!(OutputFormat::Markdown.to_string(), "markdown");
        assert_eq!(OutputFormat::StreamText.to_string(), "stream-text");
    }

    #[test]
    fn validate_request_accepts_valid_input() {
        let runner = PromptRunner::new();
        assert!(runner.validate_request(&valid_request()).is_ok());
    }

    #[test]
    fn validate_request_rejects_empty_prompt() {
        let runner = PromptRunner::new();
        let mut req = valid_request();
        req.prompt = "  ".to_string();

        let err = runner
            .validate_request(&req)
            .expect_err("empty prompt must fail");
        assert_eq!(err, PromptError::EmptyPrompt);
    }

    #[test]
    fn validate_request_rejects_unknown_model() {
        let runner = PromptRunner::new();
        let mut req = valid_request();
        req.model = Some("unknown".to_string());

        let err = runner
            .validate_request(&req)
            .expect_err("unknown model must fail");
        assert_eq!(err, PromptError::ModelNotAvailable("unknown".to_string()));
    }

    #[test]
    fn validate_request_rejects_missing_context_file() {
        let runner = PromptRunner::new();
        let mut req = valid_request();
        req.context_files = vec!["/tmp/no-such-file-othala".to_string()];

        let err = runner
            .validate_request(&req)
            .expect_err("missing file should fail");
        assert_eq!(
            err,
            PromptError::ContextFileNotFound("/tmp/no-such-file-othala".to_string())
        );
    }

    #[test]
    fn validate_request_rejects_unknown_session() {
        let runner = PromptRunner::new();
        let mut req = valid_request();
        req.session_id = Some("missing-session".to_string());

        let err = runner
            .validate_request(&req)
            .expect_err("unknown session must fail");
        assert_eq!(
            err,
            PromptError::SessionNotFound("missing-session".to_string())
        );
    }

    #[test]
    fn validate_request_accepts_registered_session() {
        let mut runner = PromptRunner::new();
        runner.register_session("session-123");

        let mut req = valid_request();
        req.session_id = Some("session-123".to_string());
        assert!(runner.validate_request(&req).is_ok());
    }

    #[test]
    fn format_output_text_returns_raw_content() {
        let response = sample_response();
        assert_eq!(response.format_output(&OutputFormat::Text), response.content);
    }

    #[test]
    fn format_output_markdown_wraps_content() {
        let response = sample_response();
        let markdown = response.format_output(&OutputFormat::Markdown);
        assert!(markdown.starts_with("## Response"));
        assert!(markdown.contains("hello world from model"));
    }

    #[test]
    fn format_output_stream_text_splits_tokens() {
        let response = sample_response();
        let stream = response.format_output(&OutputFormat::StreamText);
        assert!(stream.contains("hello\nworld\nfrom\nmodel"));
    }

    #[test]
    fn format_output_json_is_structured_payload() {
        let response = sample_response();
        let json = response.format_output(&OutputFormat::Json);
        let parsed: JsonOutput = serde_json::from_str(&json).expect("json output must parse");

        assert_eq!(parsed.content, "hello world from model");
        assert_eq!(parsed.model, "codex");
        assert_eq!(parsed.tokens.total, 25);
        assert_eq!(parsed.session_id, "sess-1");
        assert!(!parsed.timestamp.is_empty());
    }

    #[test]
    fn format_response_uses_requested_format() {
        let runner = PromptRunner::new();
        let response = sample_response();
        let rendered = runner
            .format_response(&response, &OutputFormat::Markdown)
            .expect("formatting should work");
        assert!(rendered.contains("## Response"));
    }

    #[test]
    fn build_context_reads_files_and_preserves_order() {
        let runner = PromptRunner::new();
        let file_a = std::env::temp_dir().join(format!(
            "prompt-mode-test-a-{}.txt",
            std::process::id()
        ));
        let file_b = std::env::temp_dir().join(format!(
            "prompt-mode-test-b-{}.txt",
            std::process::id()
        ));

        fs::write(&file_a, "alpha").expect("write file a");
        fs::write(&file_b, "beta").expect("write file b");

        let context = runner
            .build_context(&[
                file_a.to_string_lossy().to_string(),
                file_b.to_string_lossy().to_string(),
            ])
            .expect("context build should work");

        assert!(context.contains("alpha"));
        assert!(context.contains("beta"));
        assert!(context.find("alpha") < context.find("beta"));

        let _ = fs::remove_file(&file_a);
        let _ = fs::remove_file(&file_b);
    }

    #[test]
    fn build_context_errors_for_missing_file() {
        let runner = PromptRunner::new();
        let err = runner
            .build_context(&["/tmp/does-not-exist-prompt-mode".to_string()])
            .expect_err("missing context file should fail");
        assert_eq!(
            err,
            PromptError::ContextFileNotFound("/tmp/does-not-exist-prompt-mode".to_string())
        );
    }

    #[test]
    fn resolve_model_prefers_requested_model() {
        let runner = PromptRunner::new();
        let model = runner.resolve_model(Some("claude"));
        assert_eq!(model, "claude");
    }

    #[test]
    fn resolve_model_falls_back_to_default() {
        let runner = PromptRunner::new();
        let model = runner.resolve_model(None);
        assert_eq!(model, "codex");
    }

    #[test]
    fn prompt_structs_serialize_round_trip() {
        let req = valid_request();
        let encoded = serde_json::to_string(&req).expect("serialize request");
        let decoded: PromptRequest = serde_json::from_str(&encoded).expect("deserialize request");
        assert_eq!(decoded.prompt, req.prompt);
        assert_eq!(decoded.format, OutputFormat::Text);
    }

    #[test]
    fn token_usage_serialization_round_trip() {
        let usage = TokenUsage {
            input: 1,
            output: 2,
            total: 3,
        };
        let encoded = serde_json::to_string(&usage).expect("serialize usage");
        let decoded: TokenUsage = serde_json::from_str(&encoded).expect("deserialize usage");
        assert_eq!(decoded.total, 3);
    }

    #[test]
    fn prompt_error_display_messages() {
        assert_eq!(PromptError::EmptyPrompt.to_string(), "prompt cannot be empty");
        assert_eq!(
            PromptError::ModelNotAvailable("x".to_string()).to_string(),
            "model not available: x"
        );
    }
}
