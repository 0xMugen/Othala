use orch_core::types::{ModelKind, Task, TaskId};
use serde::{Deserialize, Serialize};

use crate::model::{AgentPaneStatus, QATestDisplay};

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
    /// QA status update for a specific task.
    #[serde(rename = "qa_update")]
    QAUpdate {
        task_id: TaskId,
        /// Status label (e.g. "baseline running", "validation: 5/5 passed").
        status: String,
        /// Per-test results from the latest QA run.
        tests: Vec<QATestDisplay>,
        /// Task-specific acceptance targets.
        targets: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    use orch_core::types::{ModelKind, RepoId, Task, TaskId};
    use std::path::PathBuf;

    use super::TuiEvent;
    use crate::AgentPaneStatus;

    fn mk_task(id: &str) -> Task {
        Task::new(
            TaskId(id.to_string()),
            RepoId("example".to_string()),
            format!("Task {id}"),
            PathBuf::from(format!(".orch/wt/{id}")),
        )
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

    #[test]
    fn qa_update_event_roundtrip() {
        use crate::QATestDisplay;

        let event = TuiEvent::QAUpdate {
            task_id: TaskId("T1".to_string()),
            status: "passed 3/3".to_string(),
            tests: vec![
                QATestDisplay {
                    name: "daemon_banner".to_string(),
                    suite: "startup".to_string(),
                    passed: true,
                    detail: String::new(),
                },
                QATestDisplay {
                    name: "create_chat".to_string(),
                    suite: "tui".to_string(),
                    passed: false,
                    detail: "timeout".to_string(),
                },
            ],
            targets: vec!["verify new endpoint".to_string()],
        };

        let encoded = serde_json::to_string(&event).expect("serialize");
        let decoded: TuiEvent = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, event);

        let value = serde_json::to_value(&event).expect("to_value");
        assert_eq!(value["kind"], "qa_update");
    }
}
