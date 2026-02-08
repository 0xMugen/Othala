use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::state::{ReviewStatus, TaskState, VerifyStatus};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RepoId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventId(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    Claude,
    Codex,
    Gemini,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmitMode {
    Single,
    Stack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskRole {
    Architecture,
    GraphiteStack,
    Docs,
    Frontend,
    Tests,
    General,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    Feature,
    Bugfix,
    Chore,
    Refactor,
    Docs,
    Test,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestRef {
    pub number: u64,
    pub url: String,
    pub draft: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSpec {
    pub repo_id: RepoId,
    pub task_id: TaskId,
    pub title: String,
    #[serde(rename = "type")]
    pub task_type: TaskType,
    pub role: TaskRole,
    pub preferred_model: Option<ModelKind>,
    #[serde(default)]
    pub depends_on: Vec<TaskId>,
    pub submit_mode: Option<SubmitMode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub repo_id: RepoId,
    pub title: String,
    pub state: TaskState,
    pub role: TaskRole,
    pub task_type: TaskType,
    pub preferred_model: Option<ModelKind>,
    pub depends_on: Vec<TaskId>,
    pub submit_mode: SubmitMode,
    pub branch_name: Option<String>,
    pub worktree_path: PathBuf,
    pub pr: Option<PullRequestRef>,
    pub verify_status: VerifyStatus,
    pub review_status: ReviewStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskApproval {
    pub task_id: TaskId,
    pub reviewer: ModelKind,
    pub verdict: crate::events::ReviewVerdict,
    pub issued_at: DateTime<Utc>,
}
