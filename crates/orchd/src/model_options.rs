use serde::{Deserialize, Serialize};
use std::env;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Low,
    #[default]
    Medium,
    High,
}

impl ReasoningEffort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            _ => None,
        }
    }
}

impl std::fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ModelOptions {
    pub reasoning_effort: Option<ReasoningEffort>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u64>,
    pub top_p: Option<f64>,
    pub stop_sequences: Vec<String>,
    pub frequency_penalty: Option<f64>,
    pub presence_penalty: Option<f64>,
    pub seed: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCapability {
    TextGeneration,
    CodeGeneration,
    Reasoning,
    Vision,
    ToolUse,
    Streaming,
}

impl ModelCapability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TextGeneration => "text_generation",
            Self::CodeGeneration => "code_generation",
            Self::Reasoning => "reasoning",
            Self::Vision => "vision",
            Self::ToolUse => "tool_use",
            Self::Streaming => "streaming",
        }
    }
}

impl std::fmt::Display for ModelCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderAuth {
    pub env_var: String,
    pub header_name: String,
    pub header_prefix: String,
}

impl ProviderAuth {
    pub fn new(env_var: impl Into<String>) -> Self {
        Self {
            env_var: env_var.into(),
            header_name: "Authorization".to_string(),
            header_prefix: "Bearer ".to_string(),
        }
    }

    pub fn is_configured(&self) -> bool {
        env::var(&self.env_var)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
    }

    pub fn get_header_value(&self) -> Option<String> {
        let token = env::var(&self.env_var).ok()?;
        if token.trim().is_empty() {
            return None;
        }
        Some(format!("{}{}", self.header_prefix, token))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelConstraints {
    pub max_temperature: f64,
    pub min_temperature: f64,
    pub max_top_p: f64,
    pub supports_reasoning_effort: bool,
    pub supports_streaming: bool,
    pub supports_tool_use: bool,
}

impl Default for ModelConstraints {
    fn default() -> Self {
        Self {
            max_temperature: 2.0,
            min_temperature: 0.0,
            max_top_p: 1.0,
            supports_reasoning_effort: false,
            supports_streaming: true,
            supports_tool_use: true,
        }
    }
}

impl ModelConstraints {
    pub fn validate_options(&self, options: &ModelOptions) -> Vec<String> {
        let mut warnings = Vec::new();

        if let Some(temperature) = options.temperature {
            if temperature < self.min_temperature || temperature > self.max_temperature {
                warnings.push(format!(
                    "temperature {temperature} is outside supported range [{}, {}]",
                    self.min_temperature, self.max_temperature
                ));
            }
        }

        if let Some(top_p) = options.top_p {
            if !(0.0..=self.max_top_p).contains(&top_p) {
                warnings.push(format!(
                    "top_p {top_p} is outside supported range [0.0, {}]",
                    self.max_top_p
                ));
            }
        }

        if options.reasoning_effort.is_some() && !self.supports_reasoning_effort {
            warnings.push("reasoning_effort is not supported for this model".to_string());
        }

        warnings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_effort_display_and_parse_roundtrip() {
        assert_eq!(ReasoningEffort::Low.to_string(), "low");
        assert_eq!(ReasoningEffort::Medium.to_string(), "medium");
        assert_eq!(ReasoningEffort::High.to_string(), "high");

        assert_eq!(ReasoningEffort::from_str("LOW"), Some(ReasoningEffort::Low));
        assert_eq!(ReasoningEffort::from_str("medium"), Some(ReasoningEffort::Medium));
        assert_eq!(ReasoningEffort::from_str("high"), Some(ReasoningEffort::High));
        assert_eq!(ReasoningEffort::from_str("invalid"), None);
    }

    #[test]
    fn reasoning_effort_serde_uses_lowercase() {
        let json = serde_json::to_string(&ReasoningEffort::High).expect("serialize effort");
        assert_eq!(json, "\"high\"");
        let effort: ReasoningEffort = serde_json::from_str("\"low\"").expect("deserialize effort");
        assert_eq!(effort, ReasoningEffort::Low);
    }

    #[test]
    fn model_options_default_is_empty() {
        let options = ModelOptions::default();
        assert!(options.reasoning_effort.is_none());
        assert!(options.temperature.is_none());
        assert!(options.max_tokens.is_none());
        assert!(options.top_p.is_none());
        assert!(options.stop_sequences.is_empty());
        assert!(options.frequency_penalty.is_none());
        assert!(options.presence_penalty.is_none());
        assert!(options.seed.is_none());
    }

    #[test]
    fn model_options_serde_roundtrip() {
        let options = ModelOptions {
            reasoning_effort: Some(ReasoningEffort::High),
            temperature: Some(0.3),
            max_tokens: Some(4096),
            top_p: Some(0.95),
            stop_sequences: vec!["END".to_string()],
            frequency_penalty: Some(0.1),
            presence_penalty: Some(0.2),
            seed: Some(42),
        };

        let json = serde_json::to_string(&options).expect("serialize options");
        let restored: ModelOptions = serde_json::from_str(&json).expect("deserialize options");
        assert_eq!(restored, options);
    }

    #[test]
    fn model_capability_display() {
        assert_eq!(ModelCapability::TextGeneration.to_string(), "text_generation");
        assert_eq!(ModelCapability::CodeGeneration.to_string(), "code_generation");
        assert_eq!(ModelCapability::Reasoning.to_string(), "reasoning");
        assert_eq!(ModelCapability::Vision.to_string(), "vision");
        assert_eq!(ModelCapability::ToolUse.to_string(), "tool_use");
        assert_eq!(ModelCapability::Streaming.to_string(), "streaming");
    }

    #[test]
    fn provider_auth_not_configured_for_missing_env_var() {
        let auth = ProviderAuth::new("OTHALA_TEST_VAR_SHOULD_NOT_EXIST");
        assert!(!auth.is_configured());
        assert!(auth.get_header_value().is_none());
    }

    #[test]
    fn provider_auth_uses_existing_env_variable() {
        let auth = ProviderAuth {
            env_var: "PATH".to_string(),
            header_name: "Authorization".to_string(),
            header_prefix: "Bearer ".to_string(),
        };

        assert!(auth.is_configured());
        let header = auth.get_header_value().expect("expected PATH value to exist");
        assert!(header.starts_with("Bearer "));
        assert!(header.len() > "Bearer ".len());
    }

    #[test]
    fn model_constraints_default_values() {
        let constraints = ModelConstraints::default();
        assert_eq!(constraints.max_temperature, 2.0);
        assert_eq!(constraints.min_temperature, 0.0);
        assert_eq!(constraints.max_top_p, 1.0);
        assert!(!constraints.supports_reasoning_effort);
        assert!(constraints.supports_streaming);
        assert!(constraints.supports_tool_use);
    }

    #[test]
    fn validate_options_accepts_valid_values() {
        let constraints = ModelConstraints::default();
        let options = ModelOptions {
            temperature: Some(1.0),
            top_p: Some(0.8),
            ..ModelOptions::default()
        };
        assert!(constraints.validate_options(&options).is_empty());
    }

    #[test]
    fn validate_options_flags_temperature_and_top_p_bounds() {
        let constraints = ModelConstraints::default();
        let options = ModelOptions {
            temperature: Some(2.5),
            top_p: Some(1.5),
            ..ModelOptions::default()
        };
        let warnings = constraints.validate_options(&options);
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0].contains("temperature"));
        assert!(warnings[1].contains("top_p"));
    }

    #[test]
    fn validate_options_flags_reasoning_effort_when_unsupported() {
        let constraints = ModelConstraints {
            supports_reasoning_effort: false,
            ..ModelConstraints::default()
        };
        let options = ModelOptions {
            reasoning_effort: Some(ReasoningEffort::High),
            ..ModelOptions::default()
        };
        let warnings = constraints.validate_options(&options);
        assert_eq!(warnings, vec!["reasoning_effort is not supported for this model"]);
    }

    #[test]
    fn validate_options_allows_reasoning_effort_when_supported() {
        let constraints = ModelConstraints {
            supports_reasoning_effort: true,
            ..ModelConstraints::default()
        };
        let options = ModelOptions {
            reasoning_effort: Some(ReasoningEffort::Low),
            ..ModelOptions::default()
        };
        assert!(constraints.validate_options(&options).is_empty());
    }
}
