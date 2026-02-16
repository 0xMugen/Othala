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
    /// A retry was scheduled after a failure.
    RetryScheduled {
        attempt: u32,
        model: String,
        reason: String,
    },
    /// AI agent process started.
    AgentSpawned {
        model: String,
    },
    /// AI agent process finished.
    AgentCompleted {
        model: String,
        success: bool,
        duration_secs: u64,
    },
    CancellationRequested {
        reason: String,
    },
    /// Retry switched to a different model.
    ModelFallback {
        from_model: String,
        to_model: String,
        reason: String,
    },
    /// Context regeneration started.
    ContextRegenStarted,
    /// Context regeneration finished.
    ContextRegenCompleted {
        success: bool,
    },
    ConfigReloaded {
        changes: String,
    },
    /// Task failed (final or non-final).
    TaskFailed {
        reason: String,
        is_final: bool,
    },
    /// Test spec was validated.
    TestSpecValidated {
        passed: bool,
        details: String,
    },
    /// Orchestrator decomposed a task into sub-tasks.
    OrchestratorDecomposed {
        sub_task_ids: Vec<String>,
    },
    /// QA run started (baseline or validation).
    QAStarted {
        qa_type: String,
    },
    /// QA run completed successfully.
    QACompleted {
        passed: u32,
        failed: u32,
        total: u32,
    },
    /// QA run found failures.
    QAFailed {
        failures: Vec<String>,
    },
    BudgetExceeded,
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

    #[test]
    fn all_event_kinds_serialize_and_deserialize() {
        let kinds: Vec<EventKind> = vec![
            EventKind::TaskCreated,
            EventKind::TaskStateChanged {
                from: "CHATTING".to_string(),
                to: "READY".to_string(),
            },
            EventKind::ParentHeadUpdated {
                parent_task_id: TaskId::new("T-parent"),
            },
            EventKind::RestackStarted,
            EventKind::RestackCompleted,
            EventKind::RestackConflict,
            EventKind::VerifyStarted,
            EventKind::VerifyCompleted { success: false },
            EventKind::ReadyReached,
            EventKind::SubmitStarted {
                mode: SubmitMode::Single,
            },
            EventKind::SubmitCompleted,
            EventKind::NeedsHuman {
                reason: "merge conflict".to_string(),
            },
            EventKind::Error {
                code: "E001".to_string(),
                message: "something broke".to_string(),
            },
            EventKind::RetryScheduled {
                attempt: 2,
                model: "claude".to_string(),
                reason: "timeout".to_string(),
            },
            EventKind::AgentSpawned {
                model: "claude".to_string(),
            },
            EventKind::AgentCompleted {
                model: "claude".to_string(),
                success: false,
                duration_secs: 42,
            },
            EventKind::CancellationRequested {
                reason: "user requested stop".to_string(),
            },
            EventKind::ModelFallback {
                from_model: "claude".to_string(),
                to_model: "codex".to_string(),
                reason: "timeout".to_string(),
            },
            EventKind::ContextRegenStarted,
            EventKind::ContextRegenCompleted { success: true },
            EventKind::ConfigReloaded {
                changes: "enabled_models, tick_interval_secs".to_string(),
            },
            EventKind::TaskFailed {
                reason: "max retries".to_string(),
                is_final: true,
            },
            EventKind::TestSpecValidated {
                passed: true,
                details: "3/3 criteria passed".to_string(),
            },
            EventKind::OrchestratorDecomposed {
                sub_task_ids: vec!["T-sub-1".to_string(), "T-sub-2".to_string()],
            },
            EventKind::QAStarted {
                qa_type: "baseline".to_string(),
            },
            EventKind::QACompleted {
                passed: 10,
                failed: 2,
                total: 12,
            },
            EventKind::QAFailed {
                failures: vec!["test_a failed".to_string(), "test_b failed".to_string()],
            },
            EventKind::BudgetExceeded,
        ];

        for kind in kinds {
            let json = serde_json::to_string(&kind).expect("serialize event kind");
            let decoded: EventKind =
                serde_json::from_str(&json).expect("deserialize event kind");
            assert_eq!(decoded, kind, "roundtrip failed for {json}");
        }
    }

    #[test]
    fn event_with_none_task_id_roundtrips() {
        let event = Event {
            id: EventId("E-global".to_string()),
            task_id: None,
            repo_id: None,
            at: Utc::now(),
            kind: EventKind::TaskCreated,
        };

        let json = serde_json::to_string(&event).unwrap();
        let decoded: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.task_id, None);
        assert_eq!(decoded.repo_id, None);
    }

    #[test]
    fn config_reloaded_event_created() {
        let kind = EventKind::ConfigReloaded {
            changes: "tick_interval_secs: 2->5".to_string(),
        };
        let encoded = serde_json::to_string(&kind).expect("serialize config reload event");
        assert!(encoded.contains("config_reloaded"));
        let decoded: EventKind = serde_json::from_str(&encoded).expect("deserialize config reload event");
        assert_eq!(decoded, kind);
    }
}
