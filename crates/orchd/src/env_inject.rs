use std::collections::HashMap;
use std::env;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvConfig {
    pub global_vars: HashMap<String, String>,
    pub per_model_vars: HashMap<String, HashMap<String, String>>,
    pub per_task_vars: HashMap<String, HashMap<String, String>>,
    pub inherit_env: bool,
    pub redact_patterns: Vec<String>,
}

impl Default for EnvConfig {
    fn default() -> Self {
        Self {
            global_vars: HashMap::new(),
            per_model_vars: HashMap::new(),
            per_task_vars: HashMap::new(),
            inherit_env: true,
            redact_patterns: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EnvInjector {
    pub config: EnvConfig,
}

impl EnvInjector {
    pub fn new(config: EnvConfig) -> Self {
        Self { config }
    }

    pub fn build_env(&self, task_id: &str, model: &str) -> HashMap<String, String> {
        let mut result = HashMap::new();

        result.extend(self.config.global_vars.clone());

        if let Some(model_vars) = self.config.per_model_vars.get(model) {
            result.extend(model_vars.clone());
        }

        if let Some(task_vars) = self.config.per_task_vars.get(task_id) {
            result.extend(task_vars.clone());
        }

        result.insert("OTHALA_TASK_ID".to_string(), task_id.to_string());
        result.insert("OTHALA_MODEL".to_string(), model.to_string());
        result.insert("OTHALA_REPO_ROOT".to_string(), Self::repo_root());
        result.insert("OTHALA_SESSION_ID".to_string(), Self::session_id());
        result.insert("OTHALA_VERSION".to_string(), env!("CARGO_PKG_VERSION").to_string());

        result
    }

    pub fn build_env_vec(&self, task_id: &str, model: &str) -> Vec<(String, String)> {
        let mut env_vec: Vec<(String, String)> = self.build_env(task_id, model).into_iter().collect();
        env_vec.sort_by(|a, b| a.0.cmp(&b.0));
        env_vec
    }

    pub fn redact_value(&self, key: &str, value: &str) -> String {
        if self.should_redact_key(key) {
            "***".to_string()
        } else {
            value.to_string()
        }
    }

    pub fn redacted_env(&self, task_id: &str, model: &str) -> HashMap<String, String> {
        self.build_env(task_id, model)
            .into_iter()
            .map(|(k, v)| {
                let redacted = self.redact_value(&k, &v);
                (k, redacted)
            })
            .collect()
    }

    pub fn merge_with_system(&self, custom: &HashMap<String, String>) -> HashMap<String, String> {
        let mut merged = if self.config.inherit_env {
            env::vars().collect::<HashMap<String, String>>()
        } else {
            HashMap::new()
        };

        merged.extend(custom.clone());
        merged
    }

    pub fn has_task_env(&self, task_id: &str) -> bool {
        self.config.per_task_vars.contains_key(task_id)
    }

    pub fn set_task_var(&mut self, task_id: &str, key: &str, value: &str) {
        let entry = self
            .config
            .per_task_vars
            .entry(task_id.to_string())
            .or_default();
        entry.insert(key.to_string(), value.to_string());
    }

    fn should_redact_key(&self, key: &str) -> bool {
        let key_upper = key.to_ascii_uppercase();
        self.config
            .redact_patterns
            .iter()
            .any(|pattern| key_upper.contains(&pattern.to_ascii_uppercase()))
    }

    fn repo_root() -> String {
        env::var("OTHALA_REPO_ROOT").unwrap_or_else(|_| {
            env::current_dir()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| ".".to_string())
        })
    }

    fn session_id() -> String {
        env::var("OTHALA_SESSION_ID").unwrap_or_else(|_| "unknown-session".to_string())
    }
}

impl Default for EnvInjector {
    fn default() -> Self {
        Self::new(EnvConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_injector() -> EnvInjector {
        let mut cfg = EnvConfig::default();
        cfg.global_vars
            .insert("SHARED".to_string(), "global".to_string());
        cfg.global_vars.insert("X".to_string(), "global-x".to_string());

        cfg.per_model_vars.insert(
            "claude".to_string(),
            HashMap::from([
                ("MODEL_ONLY".to_string(), "yes".to_string()),
                ("X".to_string(), "model-x".to_string()),
            ]),
        );

        cfg.per_task_vars.insert(
            "task-1".to_string(),
            HashMap::from([
                ("TASK_ONLY".to_string(), "ok".to_string()),
                ("X".to_string(), "task-x".to_string()),
            ]),
        );

        cfg.redact_patterns = vec!["API_KEY".to_string(), "SECRET".to_string()];
        EnvInjector::new(cfg)
    }

    #[test]
    fn env_config_defaults() {
        let cfg = EnvConfig::default();
        assert!(cfg.global_vars.is_empty());
        assert!(cfg.per_model_vars.is_empty());
        assert!(cfg.per_task_vars.is_empty());
        assert!(cfg.inherit_env);
        assert!(cfg.redact_patterns.is_empty());
    }

    #[test]
    fn build_env_merges_in_correct_precedence() {
        let injector = mk_injector();
        let env_map = injector.build_env("task-1", "claude");

        assert_eq!(env_map.get("SHARED"), Some(&"global".to_string()));
        assert_eq!(env_map.get("MODEL_ONLY"), Some(&"yes".to_string()));
        assert_eq!(env_map.get("TASK_ONLY"), Some(&"ok".to_string()));
        assert_eq!(env_map.get("X"), Some(&"task-x".to_string()));
    }

    #[test]
    fn build_env_with_unknown_task_uses_global_and_model_only() {
        let injector = mk_injector();
        let env_map = injector.build_env("task-unknown", "claude");

        assert_eq!(env_map.get("X"), Some(&"model-x".to_string()));
        assert!(!env_map.contains_key("TASK_ONLY"));
    }

    #[test]
    fn build_env_vec_contains_same_items_as_map() {
        let injector = mk_injector();
        let env_map = injector.build_env("task-1", "claude");
        let env_vec = injector.build_env_vec("task-1", "claude");

        assert_eq!(env_vec.len(), env_map.len());
        for (k, v) in env_vec {
            assert_eq!(env_map.get(&k), Some(&v));
        }
    }

    #[test]
    fn build_env_vec_is_sorted_by_key() {
        let injector = mk_injector();
        let env_vec = injector.build_env_vec("task-1", "claude");
        let keys: Vec<String> = env_vec.into_iter().map(|(k, _)| k).collect();
        let sorted = {
            let mut cloned = keys.clone();
            cloned.sort();
            cloned
        };
        assert_eq!(keys, sorted);
    }

    #[test]
    fn redact_value_masks_matching_keys() {
        let injector = mk_injector();
        assert_eq!(injector.redact_value("MY_API_KEY", "abcd"), "***");
        assert_eq!(injector.redact_value("TOP_SECRET_TOKEN", "abcd"), "***");
    }

    #[test]
    fn redact_value_preserves_non_matching_keys() {
        let injector = mk_injector();
        assert_eq!(injector.redact_value("NORMAL_VAR", "value"), "value");
    }

    #[test]
    fn redacted_env_masks_sensitive_values() {
        let mut injector = mk_injector();
        injector
            .config
            .global_vars
            .insert("SERVICE_API_KEY".to_string(), "token-123".to_string());

        let redacted = injector.redacted_env("task-1", "claude");
        assert_eq!(redacted.get("SERVICE_API_KEY"), Some(&"***".to_string()));
    }

    #[test]
    fn built_in_othala_vars_are_always_injected() {
        let injector = mk_injector();
        let env_map = injector.build_env("task-1", "claude");

        assert_eq!(env_map.get("OTHALA_TASK_ID"), Some(&"task-1".to_string()));
        assert_eq!(env_map.get("OTHALA_MODEL"), Some(&"claude".to_string()));
        assert!(env_map.contains_key("OTHALA_REPO_ROOT"));
        assert!(env_map.contains_key("OTHALA_SESSION_ID"));
        assert!(env_map.contains_key("OTHALA_VERSION"));
    }

    #[test]
    fn merge_with_system_inherits_when_enabled() {
        let cfg = EnvConfig {
            inherit_env: true,
            ..EnvConfig::default()
        };
        let injector = EnvInjector::new(cfg);
        let custom = HashMap::from([("A".to_string(), "B".to_string())]);
        let merged = injector.merge_with_system(&custom);

        assert_eq!(merged.get("A"), Some(&"B".to_string()));
        assert!(!merged.is_empty());
    }

    #[test]
    fn merge_with_system_skips_inheritance_when_disabled() {
        let cfg = EnvConfig {
            inherit_env: false,
            ..EnvConfig::default()
        };
        let injector = EnvInjector::new(cfg);
        let custom = HashMap::from([("A".to_string(), "B".to_string())]);
        let merged = injector.merge_with_system(&custom);

        assert_eq!(merged.len(), 1);
        assert_eq!(merged.get("A"), Some(&"B".to_string()));
    }

    #[test]
    fn has_task_env_reflects_presence_of_task_vars() {
        let injector = mk_injector();
        assert!(injector.has_task_env("task-1"));
        assert!(!injector.has_task_env("missing"));
    }

    #[test]
    fn set_task_var_adds_runtime_variable() {
        let mut injector = mk_injector();
        injector.set_task_var("task-runtime", "RUNTIME_KEY", "runtime-value");

        assert!(injector.has_task_env("task-runtime"));
        let env_map = injector.build_env("task-runtime", "claude");
        assert_eq!(
            env_map.get("RUNTIME_KEY"),
            Some(&"runtime-value".to_string())
        );
    }

    #[test]
    fn set_task_var_overwrites_existing_value() {
        let mut injector = mk_injector();
        injector.set_task_var("task-1", "X", "runtime-override");
        let env_map = injector.build_env("task-1", "claude");
        assert_eq!(env_map.get("X"), Some(&"runtime-override".to_string()));
    }

    #[test]
    fn built_ins_override_custom_same_name() {
        let mut cfg = EnvConfig::default();
        cfg.global_vars
            .insert("OTHALA_TASK_ID".to_string(), "bad".to_string());
        let injector = EnvInjector::new(cfg);
        let env_map = injector.build_env("task-z", "model-z");
        assert_eq!(env_map.get("OTHALA_TASK_ID"), Some(&"task-z".to_string()));
    }
}
