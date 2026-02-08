use chrono::{DateTime, Utc};
use orch_core::types::{ModelKind, RepoId, TaskId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCommand {
    pub executable: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochRequest {
    pub task_id: TaskId,
    pub repo_id: RepoId,
    pub model: ModelKind,
    pub repo_path: PathBuf,
    pub prompt: String,
    pub timeout_secs: u64,
    pub extra_args: Vec<String>,
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSignalKind {
    NeedHuman,
    PatchReady,
    RateLimited,
    ErrorHint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSignal {
    pub kind: AgentSignalKind,
    pub at: DateTime<Utc>,
    pub message: String,
    pub source_line: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PtyChunk {
    pub at: DateTime<Utc>,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpochStopReason {
    Completed,
    Failed,
    Timeout,
    NeedHuman,
    PatchReady,
    RateLimited,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochResult {
    pub task_id: TaskId,
    pub repo_id: RepoId,
    pub model: ModelKind,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub stop_reason: EpochStopReason,
    pub exit_code: Option<i32>,
    pub output: Vec<PtyChunk>,
    pub signals: Vec<AgentSignal>,
}
