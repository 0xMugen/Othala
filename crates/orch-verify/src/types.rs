use chrono::{DateTime, Utc};
use orch_core::state::VerifyTier;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyOutcome {
    Passed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyFailureClass {
    Tests,
    Lint,
    Format,
    Build,
    Environment,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedVerifyCommand {
    pub original: String,
    pub effective: String,
    pub wrapped_with_dev_shell: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyCommandResult {
    pub command: PreparedVerifyCommand,
    pub outcome: VerifyOutcome,
    pub failure_class: Option<VerifyFailureClass>,
    pub exit_code: Option<i32>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyResult {
    pub tier: VerifyTier,
    pub outcome: VerifyOutcome,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub commands: Vec<VerifyCommandResult>,
}
