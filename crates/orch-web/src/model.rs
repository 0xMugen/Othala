use chrono::{DateTime, Utc};
use orch_core::state::{ReviewStatus, TaskState, VerifyStatus};
use orch_core::types::Task;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskView {
    pub task_id: String,
    pub repo_id: String,
    pub title: String,
    pub state: TaskState,
    pub verify_status: VerifyStatus,
    pub review_status: ReviewStatus,
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
            verify_status: task.verify_status.clone(),
            review_status: task.review_status.clone(),
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
    TasksReplaced {
        count: usize,
    },
    TaskUpserted {
        task_id: String,
        state: TaskState,
    },
    SandboxUpdated {
        sandbox_id: String,
        status: SandboxStatus,
    },
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

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use orch_core::state::{
        ReviewCapacityState, ReviewStatus, TaskState, VerifyStatus, VerifyTier,
    };
    use orch_core::types::{RepoId, SubmitMode, Task, TaskId, TaskRole, TaskType};
    use std::path::PathBuf;

    use super::{web_event_name, SandboxSpawnRequest, TaskView, WebEventKind};

    #[test]
    fn task_view_carries_verify_and_review_details() {
        let task = Task {
            id: TaskId("T200".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Example task".to_string(),
            state: TaskState::Reviewing,
            role: TaskRole::General,
            task_type: TaskType::Feature,
            preferred_model: None,
            depends_on: vec![TaskId("T100".to_string())],
            submit_mode: SubmitMode::Single,
            branch_name: Some("task/T200".to_string()),
            worktree_path: PathBuf::from(".orch/wt/T200"),
            pr: None,
            verify_status: VerifyStatus::Passed {
                tier: VerifyTier::Quick,
            },
            review_status: ReviewStatus {
                required_models: vec![
                    orch_core::types::ModelKind::Claude,
                    orch_core::types::ModelKind::Codex,
                ],
                approvals_received: 1,
                approvals_required: 2,
                unanimous: true,
                capacity_state: ReviewCapacityState::Sufficient,
            },
            patch_ready: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let view = TaskView::from(&task);
        assert_eq!(view.verify_status, task.verify_status);
        assert_eq!(view.review_status, task.review_status);
        assert_eq!(view.depends_on, vec!["T100".to_string()]);
    }

    #[test]
    fn sandbox_spawn_request_defaults_cleanup_worktree_to_true() {
        let value = serde_json::json!({
            "target": { "task": { "task_id": "T1" } },
            "repo_path": "/tmp/repo",
            "nix_dev_shell": "nix develop",
            "verify_full_commands": ["echo ok"]
        });
        let request: SandboxSpawnRequest =
            serde_json::from_value(value).expect("sandbox spawn request json");
        assert!(request.cleanup_worktree);
    }

    #[test]
    fn sandbox_spawn_request_honors_explicit_cleanup_worktree_false() {
        let value = serde_json::json!({
            "target": { "task": { "task_id": "T1" } },
            "repo_path": "/tmp/repo",
            "nix_dev_shell": "nix develop",
            "verify_full_commands": ["echo ok"],
            "cleanup_worktree": false
        });
        let request: SandboxSpawnRequest =
            serde_json::from_value(value).expect("sandbox spawn request json");
        assert!(!request.cleanup_worktree);
    }

    #[test]
    fn web_event_name_maps_all_variants() {
        assert_eq!(
            web_event_name(&WebEventKind::TasksReplaced { count: 1 }),
            "tasks_replaced"
        );
        assert_eq!(
            web_event_name(&WebEventKind::TaskUpserted {
                task_id: "T1".to_string(),
                state: TaskState::Running
            }),
            "task_upserted"
        );
        assert_eq!(
            web_event_name(&WebEventKind::SandboxUpdated {
                sandbox_id: "SBX-1".to_string(),
                status: super::SandboxStatus::Running
            }),
            "sandbox_updated"
        );
    }

    #[test]
    fn sandbox_target_serializes_with_snake_case_tag() {
        let task_target = super::SandboxTarget::Task {
            task_id: "T1".to_string(),
        };
        let task_value = serde_json::to_value(&task_target).expect("serialize task target");
        assert_eq!(
            task_value,
            serde_json::json!({
                "task": {
                    "task_id": "T1"
                }
            })
        );
        let task_back: super::SandboxTarget =
            serde_json::from_value(task_value).expect("deserialize task target");
        assert_eq!(task_back, task_target);

        let stack_target = super::SandboxTarget::Stack {
            task_ids: vec!["T1".to_string(), "T2".to_string()],
        };
        let stack_value = serde_json::to_value(&stack_target).expect("serialize stack target");
        assert_eq!(
            stack_value,
            serde_json::json!({
                "stack": {
                    "task_ids": ["T1", "T2"]
                }
            })
        );
        let stack_back: super::SandboxTarget =
            serde_json::from_value(stack_value).expect("deserialize stack target");
        assert_eq!(stack_back, stack_target);
    }

    #[test]
    fn task_view_maps_pr_fields_when_present() {
        let mut task = Task {
            id: TaskId("T300".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Task with PR".to_string(),
            state: TaskState::Ready,
            role: TaskRole::General,
            task_type: TaskType::Feature,
            preferred_model: None,
            depends_on: Vec::new(),
            submit_mode: SubmitMode::Single,
            branch_name: Some("task/T300".to_string()),
            worktree_path: PathBuf::from(".orch/wt/T300"),
            pr: None,
            verify_status: VerifyStatus::NotRun,
            review_status: ReviewStatus {
                required_models: Vec::new(),
                approvals_received: 0,
                approvals_required: 0,
                unanimous: false,
                capacity_state: ReviewCapacityState::Sufficient,
            },
            patch_ready: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        task.pr = Some(orch_core::types::PullRequestRef {
            number: 123,
            url: "https://github.com/org/repo/pull/123".to_string(),
            draft: true,
        });

        let view = TaskView::from(&task);
        assert_eq!(view.pr_number, Some(123));
        assert_eq!(
            view.pr_url,
            Some("https://github.com/org/repo/pull/123".to_string())
        );
    }
}
