use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::state::ReviewPolicy;
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgConfig {
    pub models: ModelsConfig,
    pub concurrency: ConcurrencyConfig,
    pub graphite: GraphiteOrgConfig,
    pub ui: UiConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelsConfig {
    pub enabled: Vec<ModelKind>,
    pub policy: ReviewPolicy,
    pub min_approvals: usize,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyConfig {
    pub quick: VerifyCommands,
    pub full: VerifyCommands,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyCommands {
    pub commands: Vec<String>,
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
