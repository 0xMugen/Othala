//! Configuration types for the MVP orchestrator.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::types::{ModelKind, SubmitMode};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config file at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse config at {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("failed to serialize config at {path}: {source}")]
    Serialize {
        path: PathBuf,
        #[source]
        source: toml::ser::Error,
    },
    #[error("failed to create config parent directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write config file at {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum SetupApplyError {
    #[error("enabled models must not be empty")]
    EmptyEnabledModels,
    #[error("per-model concurrency default must be greater than zero")]
    InvalidConcurrencyDefault,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigProfile {
    Dev,
    Staging,
    Prod,
    Custom(String),
}

impl ConfigProfile {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Dev => "dev",
            Self::Staging => "staging",
            Self::Prod => "prod",
            Self::Custom(name) => name.as_str(),
        }
    }
}

impl Serialize for ConfigProfile {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ConfigProfile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.to_lowercase().as_str() {
            "dev" => Self::Dev,
            "staging" => Self::Staging,
            "prod" => Self::Prod,
            _ => Self::Custom(value),
        })
    }
}

/// Organization-wide configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgConfig {
    #[serde(default)]
    pub profile: Option<ConfigProfile>,
    pub models: ModelsConfig,
    pub concurrency: ConcurrencyConfig,
    pub graphite: GraphiteOrgConfig,
    pub ui: UiConfig,
    #[serde(default)]
    pub notifications: NotificationConfig,
    #[serde(default)]
    pub daemon: DaemonOrgConfig,
    #[serde(default)]
    pub budget: BudgetConfig,
}

impl Default for OrgConfig {
    fn default() -> Self {
        Self {
            profile: None,
            models: ModelsConfig {
                enabled: vec![ModelKind::Claude, ModelKind::Codex, ModelKind::Gemini],
                default: Some(ModelKind::Claude),
            },
            concurrency: ConcurrencyConfig {
                per_repo: 10,
                claude: 10,
                codex: 10,
                gemini: 10,
            },
            graphite: GraphiteOrgConfig {
                auto_submit: true,
                submit_mode_default: SubmitMode::Single,
                allow_move: MovePolicy::Manual,
            },
            ui: UiConfig {
                web_bind: "127.0.0.1:9842".to_string(),
            },
            notifications: NotificationConfig::default(),
            daemon: DaemonOrgConfig::default(),
            budget: BudgetConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_daily_token_limit")]
    pub daily_token_limit: u64,
    #[serde(default = "default_monthly_token_limit")]
    pub monthly_token_limit: u64,
}

fn default_daily_token_limit() -> u64 {
    10_000_000
}

fn default_monthly_token_limit() -> u64 {
    100_000_000
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            daily_token_limit: default_daily_token_limit(),
            monthly_token_limit: default_monthly_token_limit(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonOrgConfig {
    #[serde(default = "default_tick_interval")]
    pub tick_interval_secs: u64,
    #[serde(default = "default_agent_timeout")]
    pub agent_timeout_secs: u64,
}

fn default_tick_interval() -> u64 {
    2
}

fn default_agent_timeout() -> u64 {
    1_800
}

impl Default for DaemonOrgConfig {
    fn default() -> Self {
        Self {
            tick_interval_secs: default_tick_interval(),
            agent_timeout_secs: default_agent_timeout(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationConfig {
    pub enabled: bool,
    pub webhook_url: Option<String>,
    #[serde(default)]
    pub slack_webhook_url: Option<String>,
    #[serde(default)]
    pub slack_channel: Option<String>,
    pub stdout: bool,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            webhook_url: None,
            slack_webhook_url: None,
            slack_channel: None,
            stdout: true,
        }
    }
}

/// Model configuration - simplified for MVP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelsConfig {
    pub enabled: Vec<ModelKind>,
    /// Default model to use for new chats
    #[serde(default)]
    pub default: Option<ModelKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConcurrencyConfig {
    pub per_repo: usize,
    pub claude: usize,
    pub codex: usize,
    pub gemini: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MovePolicy {
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphiteOrgConfig {
    pub auto_submit: bool,
    pub submit_mode_default: SubmitMode,
    pub allow_move: MovePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiConfig {
    pub web_bind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoConfig {
    pub repo_id: String,
    pub repo_path: PathBuf,
    pub base_branch: String,
    pub nix: NixConfig,
    pub verify: VerifyConfig,
    pub graphite: RepoGraphiteConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NixConfig {
    pub dev_shell: String,
}

/// Verify configuration - simplified for MVP (just a command).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyConfig {
    /// Command to run for verification (e.g., "cargo check && cargo test")
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoGraphiteConfig {
    pub draft_on_start: bool,
    pub submit_mode: Option<SubmitMode>,
}

pub fn parse_org_config(contents: &str) -> Result<OrgConfig, toml::de::Error> {
    toml::from_str(contents)
}

pub fn parse_repo_config(contents: &str) -> Result<RepoConfig, toml::de::Error> {
    toml::from_str(contents)
}

pub fn load_org_config(path: impl AsRef<Path>) -> Result<OrgConfig, ConfigError> {
    let path_ref = path.as_ref();
    let body = fs::read_to_string(path_ref).map_err(|source| ConfigError::Read {
        path: path_ref.to_path_buf(),
        source,
    })?;
    parse_org_config(&body).map_err(|source| ConfigError::Parse {
        path: path_ref.to_path_buf(),
        source,
    })
}

pub fn load_repo_config(path: impl AsRef<Path>) -> Result<RepoConfig, ConfigError> {
    let path_ref = path.as_ref();
    let body = fs::read_to_string(path_ref).map_err(|source| ConfigError::Read {
        path: path_ref.to_path_buf(),
        source,
    })?;
    parse_repo_config(&body).map_err(|source| ConfigError::Parse {
        path: path_ref.to_path_buf(),
        source,
    })
}

pub fn save_org_config(path: impl AsRef<Path>, config: &OrgConfig) -> Result<(), ConfigError> {
    let path_ref = path.as_ref();
    let parent = path_ref.parent().map(Path::to_path_buf);
    if let Some(parent_dir) = parent {
        fs::create_dir_all(&parent_dir).map_err(|source| ConfigError::CreateDir {
            path: parent_dir,
            source,
        })?;
    }

    let body = toml::to_string_pretty(config).map_err(|source| ConfigError::Serialize {
        path: path_ref.to_path_buf(),
        source,
    })?;
    fs::write(path_ref, body).map_err(|source| ConfigError::Write {
        path: path_ref.to_path_buf(),
        source,
    })?;
    Ok(())
}

pub fn apply_setup_selection_to_org_config(
    config: &mut OrgConfig,
    enabled_models: &[ModelKind],
    per_model_concurrency_default: usize,
) -> Result<(), SetupApplyError> {
    if enabled_models.is_empty() {
        return Err(SetupApplyError::EmptyEnabledModels);
    }
    if per_model_concurrency_default == 0 {
        return Err(SetupApplyError::InvalidConcurrencyDefault);
    }

    config.models.enabled = dedupe_models(enabled_models);
    config.concurrency.claude = per_model_concurrency_default;
    config.concurrency.codex = per_model_concurrency_default;
    config.concurrency.gemini = per_model_concurrency_default;
    Ok(())
}

pub fn apply_profile_defaults(profile: &ConfigProfile, config: &mut OrgConfig) {
    match profile {
        ConfigProfile::Dev => {
            config.concurrency.per_repo = 20;
            config.concurrency.claude = 20;
            config.concurrency.codex = 20;
            config.concurrency.gemini = 20;
        }
        ConfigProfile::Staging => {
            config.budget.enabled = true;
        }
        ConfigProfile::Prod => {
            config.budget.enabled = true;
        }
        ConfigProfile::Custom(_) => {}
    }
}

fn dedupe_models(models: &[ModelKind]) -> Vec<ModelKind> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for model in models {
        if seen.insert(*model) {
            out.push(*model);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn sample_org() -> OrgConfig {
        parse_org_config(
            r#"
[models]
enabled = ["claude", "codex", "gemini"]

[concurrency]
per_repo = 10
claude = 10
codex = 10
gemini = 10

[graphite]
auto_submit = true
submit_mode_default = "single"
allow_move = "manual"

[ui]
web_bind = "127.0.0.1:9842"

[notifications]
enabled = false
stdout = true
"#,
        )
        .expect("parse org config")
    }

    fn sample_org_with_profile(profile: ConfigProfile) -> OrgConfig {
        let mut config = sample_org();
        config.profile = Some(profile);
        config
    }

    fn sample_repo() -> &'static str {
        r#"
repo_id = "example"
repo_path = "/home/user/src/example"
base_branch = "main"

[nix]
dev_shell = "nix develop"

[verify]
command = "cargo check && cargo test"

[graphite]
draft_on_start = true
submit_mode = "single"
"#
    }

    fn unique_temp_path(file_name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "{file_name}-{}.toml",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ))
    }

    #[test]
    fn apply_setup_selection_updates_enabled_models_and_concurrency() {
        let mut config = sample_org();
        apply_setup_selection_to_org_config(
            &mut config,
            &[ModelKind::Codex, ModelKind::Claude, ModelKind::Codex],
            7,
        )
        .expect("apply setup");

        assert_eq!(
            config.models.enabled,
            vec![ModelKind::Codex, ModelKind::Claude]
        );
        assert_eq!(config.concurrency.claude, 7);
        assert_eq!(config.concurrency.codex, 7);
        assert_eq!(config.concurrency.gemini, 7);
    }

    #[test]
    fn apply_setup_selection_validates_inputs() {
        let mut config = sample_org();
        let err = apply_setup_selection_to_org_config(&mut config, &[], 4).expect_err("empty set");
        assert!(matches!(err, SetupApplyError::EmptyEnabledModels));

        let err = apply_setup_selection_to_org_config(&mut config, &[ModelKind::Claude], 0)
            .expect_err("zero concurrency");
        assert!(matches!(err, SetupApplyError::InvalidConcurrencyDefault));
    }

    #[test]
    fn save_and_load_org_config_roundtrip() {
        let mut config = sample_org();
        apply_setup_selection_to_org_config(
            &mut config,
            &[ModelKind::Claude, ModelKind::Gemini],
            9,
        )
        .expect("apply setup");

        let path = std::env::temp_dir().join(format!(
            "othala-org-config-test-{}.toml",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));

        save_org_config(&path, &config).expect("save config");
        let loaded = load_org_config(&path).expect("load config");
        assert_eq!(loaded, config);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn parse_repo_config_parses_spec_shape() {
        let repo = parse_repo_config(sample_repo()).expect("parse repo config");
        assert_eq!(repo.repo_id, "example");
        assert_eq!(repo.base_branch, "main");
        assert_eq!(repo.nix.dev_shell, "nix develop");
        assert_eq!(repo.verify.command, "cargo check && cargo test");
        assert_eq!(repo.graphite.submit_mode, Some(SubmitMode::Single));
    }

    #[test]
    fn daemon_config_defaults() {
        let config = sample_org();
        assert_eq!(config.daemon.tick_interval_secs, 2);
        assert_eq!(config.daemon.agent_timeout_secs, 1_800);
    }

    #[test]
    fn daemon_config_custom_values() {
        let config = parse_org_config(
            r#"
[models]
enabled = ["claude"]

[concurrency]
per_repo = 5
claude = 3
codex = 1
gemini = 1

[graphite]
auto_submit = false
submit_mode_default = "single"
allow_move = "manual"

[ui]
web_bind = "127.0.0.1:9842"

[notifications]
enabled = false
stdout = true

[daemon]
tick_interval_secs = 7
agent_timeout_secs = 90
"#,
        )
        .expect("parse org config with custom daemon values");

        assert_eq!(config.daemon.tick_interval_secs, 7);
        assert_eq!(config.daemon.agent_timeout_secs, 90);
    }

    #[test]
    fn daemon_config_partial_override() {
        let config = parse_org_config(
            r#"
[models]
enabled = ["claude"]

[concurrency]
per_repo = 5
claude = 3
codex = 1
gemini = 1

[graphite]
auto_submit = false
submit_mode_default = "single"
allow_move = "manual"

[ui]
web_bind = "127.0.0.1:9842"

[notifications]
enabled = false
stdout = true

[daemon]
tick_interval_secs = 11
"#,
        )
        .expect("parse org config with partial daemon override");

        assert_eq!(config.daemon.tick_interval_secs, 11);
        assert_eq!(config.daemon.agent_timeout_secs, 1_800);
    }

    #[test]
    fn budget_config_defaults() {
        let config = sample_org();
        assert!(!config.budget.enabled);
        assert_eq!(config.budget.daily_token_limit, 10_000_000);
        assert_eq!(config.budget.monthly_token_limit, 100_000_000);
    }

    #[test]
    fn load_repo_config_classifies_read_and_parse_errors() {
        let missing_path = unique_temp_path("othala-missing-repo-config");
        let err = load_repo_config(&missing_path).expect_err("missing file should fail");
        assert!(matches!(err, ConfigError::Read { path, .. } if path == missing_path));

        let invalid_path = unique_temp_path("othala-invalid-repo-config");
        fs::write(&invalid_path, "repo_id = [").expect("write invalid repo config fixture");
        let err = load_repo_config(&invalid_path).expect_err("invalid config should fail");
        assert!(matches!(err, ConfigError::Parse { path, .. } if path == invalid_path));
        let _ = fs::remove_file(invalid_path);
    }

    #[test]
    fn apply_profile_defaults_dev_increases_concurrency_limits() {
        let mut config = sample_org();
        apply_profile_defaults(&ConfigProfile::Dev, &mut config);

        assert_eq!(config.concurrency.per_repo, 20);
        assert_eq!(config.concurrency.claude, 20);
        assert_eq!(config.concurrency.codex, 20);
        assert_eq!(config.concurrency.gemini, 20);
    }

    #[test]
    fn apply_profile_defaults_prod_enables_budget_enforcement() {
        let mut config = sample_org();
        assert!(!config.budget.enabled);

        apply_profile_defaults(&ConfigProfile::Prod, &mut config);

        assert!(config.budget.enabled);
    }

    #[test]
    fn apply_profile_defaults_custom_does_not_override_values() {
        let mut config = sample_org();
        config.concurrency.per_repo = 13;
        config.budget.enabled = false;

        apply_profile_defaults(&ConfigProfile::Custom("team-a".to_string()), &mut config);

        assert_eq!(config.concurrency.per_repo, 13);
        assert!(!config.budget.enabled);
    }

    #[test]
    fn parse_org_config_maps_profile_to_enum_variants() {
        let dev = parse_org_config(
            r#"
profile = "dev"

[models]
enabled = ["claude"]

[concurrency]
per_repo = 5
claude = 3
codex = 1
gemini = 1

[graphite]
auto_submit = false
submit_mode_default = "single"
allow_move = "manual"

[ui]
web_bind = "127.0.0.1:9842"

[notifications]
enabled = false
stdout = true
"#,
        )
        .expect("parse dev profile");
        assert_eq!(dev.profile, Some(ConfigProfile::Dev));

        let custom = parse_org_config(
            r#"
profile = "team-a"

[models]
enabled = ["claude"]

[concurrency]
per_repo = 5
claude = 3
codex = 1
gemini = 1

[graphite]
auto_submit = false
submit_mode_default = "single"
allow_move = "manual"

[ui]
web_bind = "127.0.0.1:9842"

[notifications]
enabled = false
stdout = true
"#,
        )
        .expect("parse custom profile");
        assert_eq!(custom.profile, Some(ConfigProfile::Custom("team-a".to_string())));
    }

    #[test]
    fn serialize_org_config_writes_profile_as_string() {
        let config = sample_org_with_profile(ConfigProfile::Staging);
        let serialized = toml::to_string(&config).expect("serialize org config");
        assert!(serialized.contains("profile = \"staging\""));
    }
}
