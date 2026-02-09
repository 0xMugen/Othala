use orch_core::types::{ModelKind, Task, TaskId};
use serde::{Deserialize, Serialize};

use crate::model::AgentPaneStatus;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TuiEvent {
    TasksReplaced {
        tasks: Vec<Task>,
    },
    AgentPaneOutput {
        instance_id: String,
        task_id: TaskId,
        model: ModelKind,
        lines: Vec<String>,
    },
    AgentPaneStatusChanged {
        instance_id: String,
        status: AgentPaneStatus,
    },
    StatusLine {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use orch_core::state::{ReviewCapacityState, ReviewStatus, TaskState, VerifyStatus};
    use orch_core::types::{ModelKind, RepoId, SubmitMode, Task, TaskId, TaskRole, TaskType};

    use super::TuiEvent;
    use crate::AgentPaneStatus;

    fn mk_task(id: &str) -> Task {
        Task {
            id: TaskId(id.to_string()),
            repo_id: RepoId("example".to_string()),
            title: format!("Task {id}"),
            state: TaskState::Running,
            role: TaskRole::General,
            task_type: TaskType::Feature,
            preferred_model: None,
            depends_on: Vec::new(),
            submit_mode: SubmitMode::Single,
            branch_name: Some(format!("task/{id}")),
            worktree_path: format!(".orch/wt/{id}").into(),
            pr: None,
            verify_status: VerifyStatus::NotRun,
            review_status: ReviewStatus {
                required_models: vec![ModelKind::Claude],
                approvals_received: 0,
                approvals_required: 1,
                unanimous: false,
                capacity_state: ReviewCapacityState::Sufficient,
            },
            patch_ready: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn tasks_replaced_event_serializes_with_kind_tag() {
        let event = TuiEvent::TasksReplaced {
            tasks: vec![mk_task("T1")],
        };

        let value = serde_json::to_value(&event).expect("serialize");
        assert_eq!(value["kind"], "tasks_replaced");
        assert!(value.get("tasks").is_some());

        let decoded: TuiEvent = serde_json::from_value(value).expect("deserialize");
        assert_eq!(decoded, event);
    }

    #[test]
    fn agent_output_and_status_events_roundtrip() {
        let output = TuiEvent::AgentPaneOutput {
            instance_id: "A1".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Codex,
            lines: vec!["line1".to_string(), "line2".to_string()],
        };
        let encoded_output = serde_json::to_string(&output).expect("serialize output");
        let decoded_output: TuiEvent =
            serde_json::from_str(&encoded_output).expect("deserialize output");
        assert_eq!(decoded_output, output);

        let status = TuiEvent::AgentPaneStatusChanged {
            instance_id: "A1".to_string(),
            status: AgentPaneStatus::Waiting,
        };
        let encoded_status = serde_json::to_string(&status).expect("serialize status");
        let decoded_status: TuiEvent =
            serde_json::from_str(&encoded_status).expect("deserialize status");
        assert_eq!(decoded_status, status);
    }

    #[test]
    fn status_line_event_roundtrip() {
        let event = TuiEvent::StatusLine {
            message: "ready".to_string(),
        };
        let encoded = serde_json::to_string(&event).expect("serialize");
        let decoded: TuiEvent = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, event);
    }
}
