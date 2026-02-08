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
