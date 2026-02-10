//! Configuration types for the MVP orchestrator.

use serde::{Deserialize, Serialize};
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

/// Organization-wide configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgConfig {
    pub models: ModelsConfig,
    pub concurrency: ConcurrencyConfig,
    pub graphite: GraphiteOrgConfig,
    pub ui: UiConfig,
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
"#,
        )
        .expect("parse org config")
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
}
