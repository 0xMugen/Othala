use chrono::{DateTime, Utc};
use orch_core::types::{ModelKind, RepoId, TaskId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRunRecord {
    pub run_id: String,
    pub task_id: TaskId,
    pub repo_id: RepoId,
    pub model: ModelKind,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub stop_reason: Option<String>,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRecord {
    pub artifact_id: String,
    pub task_id: TaskId,
    pub kind: String,
    pub path: String,
    pub created_at: DateTime<Utc>,
    pub metadata_json: Option<String>,
}
