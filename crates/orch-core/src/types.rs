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
    /// Set to `true` when the agent emits `[patch_ready]` or exits cleanly,
    /// indicating coding completed before any subsequent failure.
    #[serde(default)]
    pub patch_ready: bool,
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

#[cfg(test)]
mod tests {
    use super::{
        ModelKind, PullRequestRef, RepoId, SubmitMode, Task, TaskApproval, TaskId, TaskRole,
        TaskSpec, TaskType,
    };
    use crate::events::ReviewVerdict;
    use crate::state::{ReviewCapacityState, ReviewStatus, TaskState, VerifyStatus, VerifyTier};
    use chrono::{TimeZone, Utc};
    use serde::{Deserialize, Serialize};
    use std::path::PathBuf;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct SpecDoc {
        spec: TaskSpec,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct EnumDoc {
        model: ModelKind,
        mode: SubmitMode,
        role: TaskRole,
    }

    #[test]
    fn task_spec_uses_type_field_and_defaults_depends_on() {
        let doc: SpecDoc = toml::from_str(
            r#"
[spec]
repo_id = "example"
task_id = "T123"
title = "Add endpoint"
type = "feature"
role = "general"
preferred_model = "codex"
"#,
        )
        .expect("deserialize task spec");

        assert_eq!(doc.spec.task_type, TaskType::Feature);
        assert!(doc.spec.depends_on.is_empty());
        assert_eq!(doc.spec.preferred_model, Some(ModelKind::Codex));

        let encoded = toml::to_string(&doc).expect("serialize task spec");
        assert!(encoded.contains("type = \"feature\""));
    }

    #[test]
    fn core_enums_serialize_as_snake_case() {
        let doc = EnumDoc {
            model: ModelKind::Claude,
            mode: SubmitMode::Stack,
            role: TaskRole::GraphiteStack,
        };

        let encoded = toml::to_string(&doc).expect("serialize enum doc");
        assert!(encoded.contains("model = \"claude\""));
        assert!(encoded.contains("mode = \"stack\""));
        assert!(encoded.contains("role = \"graphite_stack\""));

        let decoded: EnumDoc = toml::from_str(&encoded).expect("deserialize enum doc");
        assert_eq!(decoded, doc);
    }

    #[test]
    fn task_roundtrip_preserves_core_fields_and_status() {
        let task = Task {
            id: TaskId("T200".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Implement verify runner".to_string(),
            state: TaskState::Reviewing,
            role: TaskRole::Tests,
            task_type: TaskType::Feature,
            preferred_model: Some(ModelKind::Gemini),
            depends_on: vec![TaskId("T100".to_string())],
            submit_mode: SubmitMode::Single,
            branch_name: Some("t200-verify-runner".to_string()),
            worktree_path: PathBuf::from(".orch/wt/T200"),
            pr: Some(PullRequestRef {
                number: 42,
                url: "https://github.com/0xMugen/Othala/pull/42".to_string(),
                draft: true,
            }),
            verify_status: VerifyStatus::Passed {
                tier: VerifyTier::Quick,
            },
            review_status: ReviewStatus {
                required_models: vec![ModelKind::Claude, ModelKind::Codex],
                approvals_received: 2,
                approvals_required: 2,
                unanimous: true,
                capacity_state: ReviewCapacityState::Sufficient,
            },
            patch_ready: false,
            created_at: Utc
                .with_ymd_and_hms(2026, 2, 8, 16, 10, 0)
                .single()
                .expect("valid created_at"),
            updated_at: Utc
                .with_ymd_and_hms(2026, 2, 8, 16, 12, 30)
                .single()
                .expect("valid updated_at"),
        };

        let encoded = toml::to_string(&task).expect("serialize task");
        let decoded: Task = toml::from_str(&encoded).expect("deserialize task");
        assert_eq!(decoded, task);
    }

    #[test]
    fn task_approval_roundtrip_preserves_reviewer_and_verdict() {
        let approval = TaskApproval {
            task_id: TaskId("T321".to_string()),
            reviewer: ModelKind::Codex,
            verdict: ReviewVerdict::Approve,
            issued_at: Utc
                .with_ymd_and_hms(2026, 2, 8, 17, 0, 0)
                .single()
                .expect("valid issued_at"),
        };

        let encoded = toml::to_string(&approval).expect("serialize approval");
        let decoded: TaskApproval = toml::from_str(&encoded).expect("deserialize approval");
        assert_eq!(decoded, approval);
    }
}
