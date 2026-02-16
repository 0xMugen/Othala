use orch_core::config::{PermissionRuleConfig, PermissionsConfig};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPermission {
    /// Allow without prompting
    Allow,
    /// Deny always
    Deny,
    /// Ask user before each use (default for dangerous ops)
    #[default]
    Ask,
}

impl ToolPermission {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::Ask => "ask",
        }
    }
}

impl fmt::Display for ToolPermission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for ToolPermission {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match normalize_token(value).as_str() {
            "allow" => Self::Allow,
            "deny" => Self::Deny,
            _ => Self::Ask,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCategory {
    /// File read operations
    FileRead,
    /// File write/delete operations
    FileWrite,
    /// Shell command execution
    ShellExec,
    /// Git operations (commit, push, etc.)
    GitOps,
    /// Network operations (fetch, curl, etc.)
    Network,
    /// Process management (spawn, kill)
    Process,
    /// Package installation (npm install, cargo add, etc.)
    PackageInstall,
    /// Environment variable access
    EnvAccess,
    /// Graphite operations (submit, merge)
    GraphiteOps,
    /// Custom tool category
    Custom(String),
}

impl ToolCategory {
    pub fn as_str(&self) -> &str {
        match self {
            Self::FileRead => "file_read",
            Self::FileWrite => "file_write",
            Self::ShellExec => "shell_exec",
            Self::GitOps => "git_ops",
            Self::Network => "network",
            Self::Process => "process",
            Self::PackageInstall => "package_install",
            Self::EnvAccess => "env_access",
            Self::GraphiteOps => "graphite_ops",
            Self::Custom(name) => name.as_str(),
        }
    }

    pub fn all_builtin() -> Vec<ToolCategory> {
        vec![
            ToolCategory::FileRead,
            ToolCategory::FileWrite,
            ToolCategory::ShellExec,
            ToolCategory::GitOps,
            ToolCategory::Network,
            ToolCategory::Process,
            ToolCategory::PackageInstall,
            ToolCategory::EnvAccess,
            ToolCategory::GraphiteOps,
        ]
    }
}

impl fmt::Display for ToolCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for ToolCategory {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match normalize_token(value).as_str() {
            "file_read" => Self::FileRead,
            "file_write" => Self::FileWrite,
            "shell_exec" => Self::ShellExec,
            "git_ops" => Self::GitOps,
            "network" => Self::Network,
            "process" => Self::Process,
            "package_install" => Self::PackageInstall,
            "env_access" => Self::EnvAccess,
            "graphite_ops" => Self::GraphiteOps,
            _ => Self::Custom(value.to_string()),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRule {
    pub category: ToolCategory,
    pub permission: ToolPermission,
    /// Optional glob pattern for path-based filtering (e.g., "src/**" for FileWrite)
    pub path_pattern: Option<String>,
    /// Optional description of why this rule exists
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PermissionPolicy {
    /// Default permission for unconfigured categories
    pub default_permission: ToolPermission,
    /// Per-category rules (later rules override earlier ones)
    pub rules: Vec<PermissionRule>,
    /// Per-model overrides (model name -> rules)
    pub model_overrides: HashMap<String, Vec<PermissionRule>>,
}

impl PermissionPolicy {
    pub fn new() -> Self {
        Default::default()
    }

    /// Check if an operation is allowed
    pub fn check(
        &self,
        category: &ToolCategory,
        path: Option<&str>,
        model: Option<&str>,
    ) -> ToolPermission {
        let mut effective = self.default_permission.clone();
        for rule in self.effective_rules(model) {
            if &rule.category != category {
                continue;
            }

            let path_matches = match (&rule.path_pattern, path) {
                (None, _) => true,
                (Some(_), None) => false,
                (Some(pattern), Some(candidate)) => Self::matches_path(pattern, candidate),
            };

            if path_matches {
                effective = rule.permission.clone();
            }
        }

        effective
    }

    /// Add a rule
    pub fn add_rule(&mut self, rule: PermissionRule) {
        self.rules.push(rule);
    }

    /// Remove rules for a category
    pub fn remove_rules_for(&mut self, category: &ToolCategory) {
        self.rules.retain(|rule| &rule.category != category);
        for override_rules in self.model_overrides.values_mut() {
            override_rules.retain(|rule| &rule.category != category);
        }
    }

    /// Get effective rules for a model (merging defaults + overrides)
    pub fn effective_rules(&self, model: Option<&str>) -> Vec<&PermissionRule> {
        let mut merged: Vec<&PermissionRule> = self.rules.iter().collect();
        if let Some(model_name) = model {
            if let Some(overrides) = self.model_overrides.get(model_name) {
                merged.extend(overrides.iter());
            }
        }
        merged
    }

    /// Create a permissive policy (everything allowed)
    pub fn permissive() -> Self {
        Self {
            default_permission: ToolPermission::Allow,
            rules: Vec::new(),
            model_overrides: HashMap::new(),
        }
    }

    /// Create a restrictive policy (everything denied except reads)
    pub fn restrictive() -> Self {
        Self {
            default_permission: ToolPermission::Deny,
            rules: vec![PermissionRule {
                category: ToolCategory::FileRead,
                permission: ToolPermission::Allow,
                path_pattern: None,
                reason: Some("read-only mode".to_string()),
            }],
            model_overrides: HashMap::new(),
        }
    }

    /// Create a balanced default policy
    pub fn default_policy() -> Self {
        let mut policy = Self::new();
        policy.rules = vec![
            PermissionRule {
                category: ToolCategory::FileRead,
                permission: ToolPermission::Allow,
                path_pattern: None,
                reason: None,
            },
            PermissionRule {
                category: ToolCategory::FileWrite,
                permission: ToolPermission::Ask,
                path_pattern: None,
                reason: None,
            },
            PermissionRule {
                category: ToolCategory::ShellExec,
                permission: ToolPermission::Ask,
                path_pattern: None,
                reason: None,
            },
            PermissionRule {
                category: ToolCategory::GitOps,
                permission: ToolPermission::Ask,
                path_pattern: None,
                reason: None,
            },
            PermissionRule {
                category: ToolCategory::Network,
                permission: ToolPermission::Ask,
                path_pattern: None,
                reason: None,
            },
            PermissionRule {
                category: ToolCategory::Process,
                permission: ToolPermission::Deny,
                path_pattern: None,
                reason: None,
            },
            PermissionRule {
                category: ToolCategory::PackageInstall,
                permission: ToolPermission::Ask,
                path_pattern: None,
                reason: None,
            },
            PermissionRule {
                category: ToolCategory::EnvAccess,
                permission: ToolPermission::Allow,
                path_pattern: None,
                reason: None,
            },
            PermissionRule {
                category: ToolCategory::GraphiteOps,
                permission: ToolPermission::Ask,
                path_pattern: None,
                reason: None,
            },
        ];
        policy
    }

    /// Format as human-readable table
    pub fn display_table(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("Default permission: {}\n", self.default_permission));
        out.push_str("CATEGORY          PERMISSION  PATH PATTERN  MODEL\n");
        out.push_str("--------------------------------------------------\n");

        for rule in &self.rules {
            out.push_str(&format!(
                "{:<17} {:<11} {:<13} {}\n",
                rule.category,
                rule.permission,
                rule.path_pattern.as_deref().unwrap_or("-"),
                "all"
            ));
        }

        let mut model_names: Vec<&String> = self.model_overrides.keys().collect();
        model_names.sort();
        for model in model_names {
            if let Some(rules) = self.model_overrides.get(model) {
                for rule in rules {
                    out.push_str(&format!(
                        "{:<17} {:<11} {:<13} {}\n",
                        rule.category,
                        rule.permission,
                        rule.path_pattern.as_deref().unwrap_or("-"),
                        model
                    ));
                }
            }
        }

        out
    }

    pub fn from_org_permissions(config: &PermissionsConfig) -> Self {
        let mut model_overrides: HashMap<String, Vec<PermissionRule>> = HashMap::new();
        for (model, rules) in &config.model_overrides {
            model_overrides.insert(
                model.clone(),
                rules.iter().map(PermissionRule::from).collect(),
            );
        }

        Self {
            default_permission: config.default_permission.parse().unwrap_or_default(),
            rules: config.rules.iter().map(PermissionRule::from).collect(),
            model_overrides,
        }
    }

    pub fn to_org_permissions(&self) -> PermissionsConfig {
        let mut model_overrides: HashMap<String, Vec<PermissionRuleConfig>> = HashMap::new();
        for (model, rules) in &self.model_overrides {
            model_overrides.insert(
                model.clone(),
                rules.iter().map(PermissionRuleConfig::from).collect(),
            );
        }

        PermissionsConfig {
            default_permission: self.default_permission.as_str().to_string(),
            rules: self.rules.iter().map(PermissionRuleConfig::from).collect(),
            model_overrides,
        }
    }

    /// Check if a path matches a glob pattern (simple implementation)
    fn matches_path(pattern: &str, path: &str) -> bool {
        wildcard_match(
            &normalize_path_component(pattern),
            &normalize_path_component(path),
        )
    }
}

impl From<&PermissionRuleConfig> for PermissionRule {
    fn from(value: &PermissionRuleConfig) -> Self {
        Self {
            category: value
                .category
                .parse()
                .unwrap_or(ToolCategory::Custom(value.category.clone())),
            permission: value.permission.parse().unwrap_or_default(),
            path_pattern: value.path_pattern.clone(),
            reason: value.reason.clone(),
        }
    }
}

impl From<&PermissionRule> for PermissionRuleConfig {
    fn from(value: &PermissionRule) -> Self {
        Self {
            category: value.category.to_string(),
            permission: value.permission.to_string(),
            path_pattern: value.path_pattern.clone(),
            reason: value.reason.clone(),
        }
    }
}

fn normalize_token(value: &str) -> String {
    value.trim().to_lowercase().replace('-', "_")
}

fn normalize_path_component(value: &str) -> String {
    value.trim().replace('\\', "/")
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let tokens = tokenize_glob(pattern);
    let chars: Vec<char> = text.chars().collect();
    let mut memo: HashMap<(usize, usize), bool> = HashMap::new();
    wildcard_match_impl(&tokens, &chars, 0, 0, &mut memo)
}

#[derive(Clone, Copy)]
enum GlobToken {
    Literal(char),
    AnyCharNoSlash,
    StarNoSlash,
    StarAny,
}

fn tokenize_glob(pattern: &str) -> Vec<GlobToken> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut tokens = Vec::with_capacity(chars.len());
    let mut i = 0usize;

    while i < chars.len() {
        match chars[i] {
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    tokens.push(GlobToken::StarAny);
                    i += 2;
                } else {
                    tokens.push(GlobToken::StarNoSlash);
                    i += 1;
                }
            }
            '?' => {
                tokens.push(GlobToken::AnyCharNoSlash);
                i += 1;
            }
            ch => {
                tokens.push(GlobToken::Literal(ch));
                i += 1;
            }
        }
    }

    tokens
}

fn wildcard_match_impl(
    tokens: &[GlobToken],
    chars: &[char],
    token_idx: usize,
    text_idx: usize,
    memo: &mut HashMap<(usize, usize), bool>,
) -> bool {
    if let Some(cached) = memo.get(&(token_idx, text_idx)) {
        return *cached;
    }

    let result = if token_idx == tokens.len() {
        text_idx == chars.len()
    } else {
        match tokens[token_idx] {
            GlobToken::Literal(ch) => {
                text_idx < chars.len()
                    && chars[text_idx] == ch
                    && wildcard_match_impl(tokens, chars, token_idx + 1, text_idx + 1, memo)
            }
            GlobToken::AnyCharNoSlash => {
                text_idx < chars.len()
                    && chars[text_idx] != '/'
                    && wildcard_match_impl(tokens, chars, token_idx + 1, text_idx + 1, memo)
            }
            GlobToken::StarNoSlash => {
                wildcard_match_impl(tokens, chars, token_idx + 1, text_idx, memo)
                    || (text_idx < chars.len()
                        && chars[text_idx] != '/'
                        && wildcard_match_impl(tokens, chars, token_idx, text_idx + 1, memo))
            }
            GlobToken::StarAny => {
                wildcard_match_impl(tokens, chars, token_idx + 1, text_idx, memo)
                    || (text_idx < chars.len()
                        && wildcard_match_impl(tokens, chars, token_idx, text_idx + 1, memo))
            }
        }
    };

    memo.insert((token_idx, text_idx), result);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_permission_is_ask() {
        let permission = ToolPermission::default();
        assert_eq!(permission, ToolPermission::Ask);
    }

    #[test]
    fn permissive_policy_allows_everything() {
        let policy = PermissionPolicy::permissive();
        assert_eq!(
            policy.check(&ToolCategory::ShellExec, Some("scripts/build.sh"), None),
            ToolPermission::Allow
        );
        assert_eq!(
            policy.check(&ToolCategory::Process, None, Some("codex")),
            ToolPermission::Allow
        );
    }

    #[test]
    fn restrictive_policy_denies_write_and_exec() {
        let policy = PermissionPolicy::restrictive();
        assert_eq!(
            policy.check(&ToolCategory::FileRead, Some("src/main.rs"), None),
            ToolPermission::Allow
        );
        assert_eq!(
            policy.check(&ToolCategory::FileWrite, Some("src/main.rs"), None),
            ToolPermission::Deny
        );
        assert_eq!(policy.check(&ToolCategory::ShellExec, None, None), ToolPermission::Deny);
    }

    #[test]
    fn add_rule_and_check_uses_latest_rule() {
        let mut policy = PermissionPolicy::new();
        policy.add_rule(PermissionRule {
            category: ToolCategory::Network,
            permission: ToolPermission::Deny,
            path_pattern: None,
            reason: None,
        });
        policy.add_rule(PermissionRule {
            category: ToolCategory::Network,
            permission: ToolPermission::Allow,
            path_pattern: None,
            reason: None,
        });

        assert_eq!(
            policy.check(&ToolCategory::Network, None, None),
            ToolPermission::Allow
        );
    }

    #[test]
    fn path_pattern_matching_works() {
        let mut policy = PermissionPolicy::new();
        policy.add_rule(PermissionRule {
            category: ToolCategory::FileWrite,
            permission: ToolPermission::Deny,
            path_pattern: Some("src/**".to_string()),
            reason: None,
        });

        assert_eq!(
            policy.check(&ToolCategory::FileWrite, Some("src/lib.rs"), None),
            ToolPermission::Deny
        );
        assert_eq!(
            policy.check(&ToolCategory::FileWrite, Some("tests/lib.rs"), None),
            ToolPermission::Ask
        );
    }

    #[test]
    fn model_overrides_apply_after_global_rules() {
        let mut policy = PermissionPolicy::new();
        policy.add_rule(PermissionRule {
            category: ToolCategory::ShellExec,
            permission: ToolPermission::Deny,
            path_pattern: None,
            reason: None,
        });
        policy.model_overrides.insert(
            "codex".to_string(),
            vec![PermissionRule {
                category: ToolCategory::ShellExec,
                permission: ToolPermission::Allow,
                path_pattern: None,
                reason: None,
            }],
        );

        assert_eq!(
            policy.check(&ToolCategory::ShellExec, None, Some("codex")),
            ToolPermission::Allow
        );
        assert_eq!(
            policy.check(&ToolCategory::ShellExec, None, Some("claude")),
            ToolPermission::Deny
        );
    }

    #[test]
    fn tool_category_parses_from_builtin_strings() {
        assert_eq!(
            "file_read".parse::<ToolCategory>().expect("parse category"),
            ToolCategory::FileRead
        );
        assert_eq!(
            "shell-exec"
                .parse::<ToolCategory>()
                .expect("parse category"),
            ToolCategory::ShellExec
        );
    }

    #[test]
    fn tool_category_unknown_becomes_custom() {
        let parsed = "my_special_tool"
            .parse::<ToolCategory>()
            .expect("parse category");
        assert_eq!(parsed, ToolCategory::Custom("my_special_tool".to_string()));
    }

    #[test]
    fn display_table_contains_expected_headers() {
        let policy = PermissionPolicy::default_policy();
        let table = policy.display_table();
        assert!(table.contains("Default permission: ask"));
        assert!(table.contains("CATEGORY"));
        assert!(table.contains("file_read"));
    }

    #[test]
    fn remove_rules_for_clears_global_and_model_specific_rules() {
        let mut policy = PermissionPolicy::new();
        policy.add_rule(PermissionRule {
            category: ToolCategory::Network,
            permission: ToolPermission::Deny,
            path_pattern: None,
            reason: None,
        });
        policy.model_overrides.insert(
            "claude".to_string(),
            vec![PermissionRule {
                category: ToolCategory::Network,
                permission: ToolPermission::Allow,
                path_pattern: None,
                reason: None,
            }],
        );

        policy.remove_rules_for(&ToolCategory::Network);

        assert_eq!(policy.rules.len(), 0);
        assert!(policy
            .model_overrides
            .get("claude")
            .is_some_and(|rules| rules.is_empty()));
    }

    #[test]
    fn effective_rules_merges_default_and_override_sets() {
        let mut policy = PermissionPolicy::new();
        policy.add_rule(PermissionRule {
            category: ToolCategory::FileRead,
            permission: ToolPermission::Allow,
            path_pattern: None,
            reason: None,
        });
        policy.model_overrides.insert(
            "gemini".to_string(),
            vec![PermissionRule {
                category: ToolCategory::ShellExec,
                permission: ToolPermission::Ask,
                path_pattern: None,
                reason: None,
            }],
        );

        let merged = policy.effective_rules(Some("gemini"));
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn org_config_roundtrip_conversion_preserves_permissions() {
        let policy = PermissionPolicy::default_policy();
        let encoded = policy.to_org_permissions();
        let decoded = PermissionPolicy::from_org_permissions(&encoded);

        assert_eq!(decoded.default_permission, ToolPermission::Ask);
        assert_eq!(decoded.rules.len(), policy.rules.len());
    }

    #[test]
    fn wildcard_match_supports_prefix_suffix_and_double_star() {
        assert!(PermissionPolicy::matches_path("src/**", "src/bin/main.rs"));
        assert!(PermissionPolicy::matches_path("*.rs", "main.rs"));
        assert!(PermissionPolicy::matches_path("docs/*/index.md", "docs/api/index.md"));
        assert!(!PermissionPolicy::matches_path("src/*.rs", "src/bin/main.rs"));
    }
}
