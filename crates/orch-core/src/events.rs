//! Event types for the MVP orchestrator.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::{EventId, RepoId, SubmitMode, TaskId};

/// Simplified event kinds for MVP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// Task created
    TaskCreated,
    /// Task state changed
    TaskStateChanged { from: String, to: String },
    /// Parent task's HEAD was updated (triggers restack)
    ParentHeadUpdated { parent_task_id: TaskId },
    /// Restack started
    RestackStarted,
    /// Restack completed successfully
    RestackCompleted,
    /// Restack has conflicts
    RestackConflict,
    /// Verify started
    VerifyStarted,
    /// Verify completed
    VerifyCompleted { success: bool },
    /// Task is ready to submit
    ReadyReached,
    /// Submit started
    SubmitStarted { mode: SubmitMode },
    /// Submit completed
    SubmitCompleted,
    /// Task needs human intervention
    NeedsHuman { reason: String },
    /// Error occurred
    Error { code: String, message: String },
}

/// An event in the orchestrator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    pub task_id: Option<TaskId>,
    pub repo_id: Option<RepoId>,
    pub at: DateTime<Utc>,
    pub kind: EventKind,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn event_kind_serializes_with_snake_case_variant_names() {
        let kind = EventKind::SubmitStarted {
            mode: SubmitMode::Stack,
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(json.contains("submit_started"));
    }

    #[test]
    fn event_roundtrip() {
        let event = Event {
            id: EventId("E100".to_string()),
            task_id: Some(TaskId::new("T200")),
            repo_id: Some(RepoId("example".to_string())),
            at: Utc
                .with_ymd_and_hms(2026, 2, 8, 12, 30, 45)
                .single()
                .expect("valid timestamp"),
            kind: EventKind::VerifyCompleted { success: true },
        };

        let json = serde_json::to_string(&event).unwrap();
        let decoded: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, event);
    }
}
