use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub provider: String,
    pub display_name: String,
    pub context_window: u64,
    pub max_output_tokens: u64,
    pub input_price_per_mtok: f64,
    pub output_price_per_mtok: f64,
    pub supports_images: bool,
    pub supports_tools: bool,
    pub supports_streaming: bool,
    pub deprecated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub name: String,
    pub display_name: String,
    pub api_base: String,
    pub auth_env_var: String,
    pub models: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelRegistry {
    pub models: HashMap<String, ModelInfo>,
    pub providers: HashMap<String, ProviderInfo>,
    pub last_updated: Option<String>,
    pub version: String,
}

impl ModelRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            models: HashMap::new(),
            providers: HashMap::new(),
            last_updated: None,
            version: "1.0.0".to_string(),
        };

        registry.providers.insert(
            "anthropic".to_string(),
            ProviderInfo {
                name: "anthropic".to_string(),
                display_name: "Anthropic".to_string(),
                api_base: "https://api.anthropic.com".to_string(),
                auth_env_var: "ANTHROPIC_API_KEY".to_string(),
                models: Vec::new(),
            },
        );
        registry.providers.insert(
            "openai".to_string(),
            ProviderInfo {
                name: "openai".to_string(),
                display_name: "OpenAI".to_string(),
                api_base: "https://api.openai.com".to_string(),
                auth_env_var: "OPENAI_API_KEY".to_string(),
                models: Vec::new(),
            },
        );
        registry.providers.insert(
            "google".to_string(),
            ProviderInfo {
                name: "google".to_string(),
                display_name: "Google".to_string(),
                api_base: "https://generativelanguage.googleapis.com".to_string(),
                auth_env_var: "GOOGLE_API_KEY".to_string(),
                models: Vec::new(),
            },
        );

        registry.insert_model(ModelInfo {
            id: "claude-sonnet-4-20250514".to_string(),
            provider: "anthropic".to_string(),
            display_name: "Claude Sonnet 4".to_string(),
            context_window: 200_000,
            max_output_tokens: 32_000,
            input_price_per_mtok: 3.0,
            output_price_per_mtok: 15.0,
            supports_images: true,
            supports_tools: true,
            supports_streaming: true,
            deprecated: false,
        });
        registry.insert_model(ModelInfo {
            id: "claude-opus-4-20250514".to_string(),
            provider: "anthropic".to_string(),
            display_name: "Claude Opus 4".to_string(),
            context_window: 200_000,
            max_output_tokens: 32_000,
            input_price_per_mtok: 15.0,
            output_price_per_mtok: 75.0,
            supports_images: true,
            supports_tools: true,
            supports_streaming: true,
            deprecated: false,
        });
        registry.insert_model(ModelInfo {
            id: "claude-haiku-3.5".to_string(),
            provider: "anthropic".to_string(),
            display_name: "Claude Haiku 3.5".to_string(),
            context_window: 200_000,
            max_output_tokens: 8_192,
            input_price_per_mtok: 0.80,
            output_price_per_mtok: 4.0,
            supports_images: true,
            supports_tools: true,
            supports_streaming: true,
            deprecated: false,
        });
        registry.insert_model(ModelInfo {
            id: "codex".to_string(),
            provider: "openai".to_string(),
            display_name: "Codex".to_string(),
            context_window: 128_000,
            max_output_tokens: 16_384,
            input_price_per_mtok: 1.5,
            output_price_per_mtok: 6.0,
            supports_images: false,
            supports_tools: true,
            supports_streaming: true,
            deprecated: false,
        });
        registry.insert_model(ModelInfo {
            id: "gemini-2.5-pro".to_string(),
            provider: "google".to_string(),
            display_name: "Gemini 2.5 Pro".to_string(),
            context_window: 1_000_000,
            max_output_tokens: 65_536,
            input_price_per_mtok: 1.25,
            output_price_per_mtok: 10.0,
            supports_images: true,
            supports_tools: true,
            supports_streaming: true,
            deprecated: false,
        });
        registry.insert_model(ModelInfo {
            id: "gemini-2.5-flash".to_string(),
            provider: "google".to_string(),
            display_name: "Gemini 2.5 Flash".to_string(),
            context_window: 1_000_000,
            max_output_tokens: 65_536,
            input_price_per_mtok: 0.15,
            output_price_per_mtok: 0.60,
            supports_images: true,
            supports_tools: true,
            supports_streaming: true,
            deprecated: false,
        });

        registry
    }

    pub fn get_model(&self, id: &str) -> Option<&ModelInfo> {
        self.models.get(id)
    }

    pub fn get_provider(&self, name: &str) -> Option<&ProviderInfo> {
        self.providers.get(name)
    }

    pub fn list_models(&self) -> Vec<&ModelInfo> {
        let mut models: Vec<&ModelInfo> = self.models.values().collect();
        models.sort_by_key(|m| m.id.as_str());
        models
    }

    pub fn list_providers(&self) -> Vec<&ProviderInfo> {
        let mut providers: Vec<&ProviderInfo> = self.providers.values().collect();
        providers.sort_by_key(|p| p.name.as_str());
        providers
    }

    pub fn models_for_provider(&self, provider: &str) -> Vec<&ModelInfo> {
        let mut models: Vec<&ModelInfo> = self
            .models
            .values()
            .filter(|m| m.provider == provider)
            .collect();
        models.sort_by_key(|m| m.id.as_str());
        models
    }

    pub fn estimate_cost(&self, model_id: &str, input_tokens: u64, output_tokens: u64) -> Option<f64> {
        let model = self.models.get(model_id)?;
        let input_cost = (input_tokens as f64 / 1_000_000.0) * model.input_price_per_mtok;
        let output_cost = (output_tokens as f64 / 1_000_000.0) * model.output_price_per_mtok;
        Some(input_cost + output_cost)
    }

    pub fn context_window(&self, model_id: &str) -> Option<u64> {
        self.models.get(model_id).map(|m| m.context_window)
    }

    #[allow(clippy::collapsible_else_if)]
    pub fn update_from_json(&mut self, json: &str) -> Result<usize, String> {
        let value: serde_json::Value =
            serde_json::from_str(json).map_err(|e| format!("invalid JSON payload: {e}"))?;

        let models_node = value.get("models").unwrap_or(&value);
        let parsed_models = parse_models(models_node)?;

        if let Some(providers_node) = value.get("providers") {
            merge_provider_payload(&mut self.providers, providers_node)?;
        }

        if let Some(version) = value.get("version").and_then(serde_json::Value::as_str) {
            self.version = version.to_string();
        }
        if let Some(updated) = value
            .get("last_updated")
            .and_then(serde_json::Value::as_str)
        {
            self.last_updated = Some(updated.to_string());
        }

        let mut added = 0usize;
        for model in parsed_models {
            if model.id.trim().is_empty() {
                return Err("model id cannot be empty".to_string());
            }

            let model_id = model.id.clone();
            let provider_name = model.provider.clone();
            let is_new = !self.models.contains_key(&model_id);

            if is_new {
                self.insert_model(model);
                added += 1;
            } else {
                if let Some(provider) = self.providers.get_mut(&provider_name) {
                    if !provider.models.contains(&model_id) {
                        provider.models.push(model_id);
                    }
                }
            }
        }

        Ok(added)
    }

    pub fn save_to_file(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("failed to serialize registry: {e}"))?;
        fs::write(path, json).map_err(|e| format!("failed to write registry file: {e}"))
    }

    pub fn load_from_file(path: &Path) -> Result<Self, String> {
        let content =
            fs::read_to_string(path).map_err(|e| format!("failed to read registry file: {e}"))?;
        serde_json::from_str(&content).map_err(|e| format!("failed to parse registry file: {e}"))
    }

    pub fn check_for_updates(&self) -> Option<String> {
        let output = Command::new("gh")
            .args([
                "api",
                "repos/0xMugen/Othala/releases/latest",
                "--jq",
                ".tag_name",
            ])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let raw_tag = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let latest = raw_tag.strip_prefix('v').unwrap_or(&raw_tag);
        if latest.is_empty() {
            return None;
        }

        if version_is_newer(latest, &self.version) {
            Some(latest.to_string())
        } else {
            None
        }
    }

    pub fn display_table(&self) -> String {
        let mut out = String::new();
        out.push_str("MODEL ID                  PROVIDER   CONTEXT   MAX OUT   INPUT $/M   OUTPUT $/M   CAPS\n");
        out.push_str("---------------------------------------------------------------------------------------\n");

        for model in self.list_models() {
            let mut caps = String::new();
            if model.supports_images {
                caps.push('I');
            }
            if model.supports_tools {
                caps.push('T');
            }
            if model.supports_streaming {
                caps.push('S');
            }
            if caps.is_empty() {
                caps.push('-');
            }

            out.push_str(&format!(
                "{:<25} {:<10} {:>8} {:>9} {:>10.2} {:>11.2}   {}{}\n",
                model.id,
                model.provider,
                model.context_window,
                model.max_output_tokens,
                model.input_price_per_mtok,
                model.output_price_per_mtok,
                caps,
                if model.deprecated { " (deprecated)" } else { "" },
            ));
        }

        out
    }

    fn insert_model(&mut self, model: ModelInfo) {
        let provider_name = model.provider.clone();
        let model_id = model.id.clone();
        self.models.insert(model_id.clone(), model);

        let provider = self
            .providers
            .entry(provider_name.clone())
            .or_insert_with(|| ProviderInfo {
                name: provider_name.clone(),
                display_name: provider_name,
                api_base: String::new(),
                auth_env_var: String::new(),
                models: Vec::new(),
            });

        if !provider.models.contains(&model_id) {
            provider.models.push(model_id);
            provider.models.sort();
        }
    }
}

fn parse_models(node: &serde_json::Value) -> Result<Vec<ModelInfo>, String> {
    match node {
        serde_json::Value::Array(_) => serde_json::from_value(node.clone())
            .map_err(|e| format!("invalid models array payload: {e}")),
        serde_json::Value::Object(map) => {
            let mut models = Vec::with_capacity(map.len());
            for (id, value) in map {
                let mut model: ModelInfo = serde_json::from_value(value.clone())
                    .map_err(|e| format!("invalid model entry '{id}': {e}"))?;
                if model.id.trim().is_empty() {
                    model.id = id.clone();
                }
                models.push(model);
            }
            Ok(models)
        }
        _ => Err("models payload must be an array or object".to_string()),
    }
}

fn merge_provider_payload(
    providers: &mut HashMap<String, ProviderInfo>,
    node: &serde_json::Value,
) -> Result<(), String> {
    match node {
        serde_json::Value::Array(list) => {
            for provider_value in list {
                let provider: ProviderInfo = serde_json::from_value(provider_value.clone())
                    .map_err(|e| format!("invalid provider entry: {e}"))?;
                providers.entry(provider.name.clone()).or_insert(provider);
            }
            Ok(())
        }
        serde_json::Value::Object(map) => {
            for (name, provider_value) in map {
                let mut provider: ProviderInfo = serde_json::from_value(provider_value.clone())
                    .map_err(|e| format!("invalid provider entry '{name}': {e}"))?;
                if provider.name.trim().is_empty() {
                    provider.name = name.clone();
                }
                providers.entry(provider.name.clone()).or_insert(provider);
            }
            Ok(())
        }
        _ => Err("providers payload must be an array or object".to_string()),
    }
}

fn version_is_newer(candidate: &str, current: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.split('.')
            .map(|segment| segment.parse::<u64>().unwrap_or(0))
            .collect()
    };

    let candidate_parts = parse(candidate);
    let current_parts = parse(current);
    let max_len = candidate_parts.len().max(current_parts.len());

    for idx in 0..max_len {
        let candidate_segment = candidate_parts.get(idx).copied().unwrap_or(0);
        let current_segment = current_parts.get(idx).copied().unwrap_or(0);
        if candidate_segment > current_segment {
            return true;
        }
        if candidate_segment < current_segment {
            return false;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn new_creates_default_registry_with_models() {
        let registry = ModelRegistry::new();
        assert!(registry.models.len() >= 6);
        assert!(registry.models.contains_key("claude-sonnet-4-20250514"));
        assert!(registry.models.contains_key("gemini-2.5-pro"));
    }

    #[test]
    fn get_model_returns_correct_info() {
        let registry = ModelRegistry::new();
        let model = registry
            .get_model("claude-sonnet-4-20250514")
            .expect("expected model");
        assert_eq!(model.provider, "anthropic");
        assert_eq!(model.context_window, 200_000);
    }

    #[test]
    fn get_model_unknown_returns_none() {
        let registry = ModelRegistry::new();
        assert!(registry.get_model("does-not-exist").is_none());
    }

    #[test]
    fn get_provider_returns_correct_info() {
        let registry = ModelRegistry::new();
        let provider = registry.get_provider("anthropic").expect("expected provider");
        assert_eq!(provider.display_name, "Anthropic");
        assert_eq!(provider.auth_env_var, "ANTHROPIC_API_KEY");
    }

    #[test]
    fn list_models_returns_all() {
        let registry = ModelRegistry::new();
        let listed = registry.list_models();
        assert_eq!(listed.len(), registry.models.len());
        assert!(listed.windows(2).all(|w| w[0].id <= w[1].id));
    }

    #[test]
    fn list_providers_returns_all() {
        let registry = ModelRegistry::new();
        let listed = registry.list_providers();
        assert_eq!(listed.len(), registry.providers.len());
        assert!(listed.windows(2).all(|w| w[0].name <= w[1].name));
    }

    #[test]
    fn models_for_provider_filters_correctly() {
        let registry = ModelRegistry::new();
        let models = registry.models_for_provider("anthropic");
        assert_eq!(models.len(), 3);
        assert!(models.iter().all(|m| m.provider == "anthropic"));
    }

    #[test]
    fn estimate_cost_calculation() {
        let registry = ModelRegistry::new();
        let cost = registry
            .estimate_cost("claude-sonnet-4-20250514", 500_000, 100_000)
            .expect("expected cost");
        assert!((cost - 3.0).abs() < 1e-9);
    }

    #[test]
    fn estimate_cost_unknown_model() {
        let registry = ModelRegistry::new();
        assert!(registry.estimate_cost("missing", 1_000, 1_000).is_none());
    }

    #[test]
    fn context_window_returns_correct_value() {
        let registry = ModelRegistry::new();
        assert_eq!(registry.context_window("gemini-2.5-pro"), Some(1_000_000));
    }

    #[test]
    fn update_from_json_adds_new_models() {
        let mut registry = ModelRegistry::new();
        let payload = r#"
        {
            "models": [
                {
                    "id": "grok-4",
                    "provider": "xai",
                    "display_name": "Grok 4",
                    "context_window": 256000,
                    "max_output_tokens": 8192,
                    "input_price_per_mtok": 5.0,
                    "output_price_per_mtok": 15.0,
                    "supports_images": true,
                    "supports_tools": true,
                    "supports_streaming": true,
                    "deprecated": false
                }
            ],
            "version": "1.1.0",
            "last_updated": "2026-01-01T00:00:00Z"
        }
        "#;

        let added = registry
            .update_from_json(payload)
            .expect("expected update to succeed");

        assert_eq!(added, 1);
        assert!(registry.get_model("grok-4").is_some());
        assert!(registry.get_provider("xai").is_some());
        assert_eq!(registry.version, "1.1.0");
        assert_eq!(registry.last_updated.as_deref(), Some("2026-01-01T00:00:00Z"));
    }

    #[test]
    fn display_table_formatting() {
        let registry = ModelRegistry::new();
        let table = registry.display_table();
        assert!(table.contains("MODEL ID"));
        assert!(table.contains("PROVIDER"));
        assert!(table.contains("claude-sonnet-4-20250514"));
    }

    #[test]
    fn save_and_load_roundtrip() {
        let registry = ModelRegistry::new();
        let mut path = std::env::temp_dir();
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        path.push(format!("orchd-provider-registry-{nonce}.json"));

        registry
            .save_to_file(&path)
            .expect("expected save to file to succeed");
        let loaded = ModelRegistry::load_from_file(&path).expect("expected load to succeed");

        assert_eq!(loaded.models.len(), registry.models.len());
        assert!(loaded.models.contains_key("codex"));

        fs::remove_file(path).expect("expected temp file cleanup to succeed");
    }
}
