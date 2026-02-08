use chrono::{DateTime, Utc};
use orch_core::state::TaskState;
use orch_core::types::Task;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskView {
    pub task_id: String,
    pub repo_id: String,
    pub title: String,
    pub state: TaskState,
    pub branch: Option<String>,
    pub worktree_path: PathBuf,
    pub pr_number: Option<u64>,
    pub pr_url: Option<String>,
    pub depends_on: Vec<String>,
    pub updated_at: DateTime<Utc>,
}

impl From<&Task> for TaskView {
    fn from(task: &Task) -> Self {
        Self {
            task_id: task.id.0.clone(),
            repo_id: task.repo_id.0.clone(),
            title: task.title.clone(),
            state: task.state,
            branch: task.branch_name.clone(),
            worktree_path: task.worktree_path.clone(),
            pr_number: task.pr.as_ref().map(|x| x.number),
            pr_url: task.pr.as_ref().map(|x| x.url.clone()),
            depends_on: task.depends_on.iter().map(|x| x.0.clone()).collect(),
            updated_at: task.updated_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskListResponse {
    pub tasks: Vec<TaskView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDetailResponse {
    pub task: TaskView,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeQueueResponse {
    pub generated_at: DateTime<Utc>,
    pub groups: Vec<MergeQueueGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeQueueGroup {
    pub group_id: String,
    pub task_ids: Vec<String>,
    pub recommended_merge_order: Vec<String>,
    pub pr_urls: Vec<String>,
    pub contains_cycle: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxTarget {
    Task { task_id: String },
    Stack { task_ids: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxSpawnRequest {
    pub target: SandboxTarget,
    pub repo_path: PathBuf,
    pub nix_dev_shell: String,
    pub verify_full_commands: Vec<String>,
    #[serde(default)]
    pub checkout_ref: Option<String>,
    #[serde(default = "default_cleanup_worktree")]
    pub cleanup_worktree: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxStatus {
    Queued,
    Running,
    Passed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxCommandLog {
    pub command: String,
    pub effective_command: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxRunView {
    pub sandbox_id: String,
    pub target: SandboxTarget,
    pub status: SandboxStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub sandbox_path: Option<PathBuf>,
    pub checkout_ref: Option<String>,
    pub cleanup_worktree: bool,
    pub worktree_cleaned: bool,
    pub worktree_cleanup_error: Option<String>,
    pub logs: Vec<SandboxCommandLog>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxSpawnResponse {
    pub sandbox_id: String,
    pub status: SandboxStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxDetailResponse {
    pub sandbox: SandboxRunView,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebEventKind {
    TasksReplaced { count: usize },
    TaskUpserted { task_id: String, state: TaskState },
    SandboxUpdated { sandbox_id: String, status: SandboxStatus },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebEvent {
    pub at: DateTime<Utc>,
    pub kind: WebEventKind,
}

pub fn web_event_name(kind: &WebEventKind) -> &'static str {
    match kind {
        WebEventKind::TasksReplaced { .. } => "tasks_replaced",
        WebEventKind::TaskUpserted { .. } => "task_upserted",
        WebEventKind::SandboxUpdated { .. } => "sandbox_updated",
    }
}

fn default_cleanup_worktree() -> bool {
    true
}
