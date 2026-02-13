//! MVP simplified state machine.
//!
//! This replaces the complex 16-state TaskState with 6 MVP states.

use serde::{Deserialize, Serialize};

/// MVP task states - simplified from 16 to 6.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskState {
    /// Active AI conversation working on code
    Chatting,
    /// Chat complete, code verified, ready to submit
    Ready,
    /// Submitting to Graphite
    Submitting,
    /// Rebasing onto updated parent (auto-restack)
    Restacking,
    /// PR submitted, waiting for merge
    AwaitingMerge,
    /// PR merged, done
    Merged,
    /// Agent exhausted all retries or was manually stopped
    Stopped,
}

impl std::fmt::Display for TaskState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tag = match self {
            TaskState::Chatting => "CHATTING",
            TaskState::Ready => "READY",
            TaskState::Submitting => "SUBMITTING",
            TaskState::Restacking => "RESTACKING",
            TaskState::AwaitingMerge => "AWAITING_MERGE",
            TaskState::Merged => "MERGED",
            TaskState::Stopped => "STOPPED",
        };
        f.write_str(tag)
    }
}

impl TaskState {
    /// Returns true if the task is in a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(self, TaskState::Merged | TaskState::Stopped)
    }

    /// Returns true if the task is actively working.
    pub fn is_active(&self) -> bool {
        matches!(self, TaskState::Chatting)
    }

    /// Returns true if the task is ready to submit.
    pub fn can_submit(&self) -> bool {
        matches!(self, TaskState::Ready)
    }

    /// Returns true if the task can be restacked.
    pub fn can_restack(&self) -> bool {
        matches!(
            self,
            TaskState::Ready | TaskState::Submitting | TaskState::AwaitingMerge
        )
    }
}

/// Simple verify result - pass or fail with optional message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VerifyStatus {
    #[default]
    NotRun,
    Running,
    Passed,
    Failed {
        message: String,
    },
}

impl VerifyStatus {
    pub fn is_passed(&self) -> bool {
        matches!(self, VerifyStatus::Passed)
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, VerifyStatus::Failed { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_state_serializes_as_screaming_snake_case() {
        let state = TaskState::Chatting;
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(json, "\"CHATTING\"");

        let state = TaskState::AwaitingMerge;
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(json, "\"AWAITING_MERGE\"");
    }

    #[test]
    fn terminal_state_check() {
        assert!(!TaskState::Chatting.is_terminal());
        assert!(!TaskState::Ready.is_terminal());
        assert!(TaskState::Merged.is_terminal());
    }

    #[test]
    fn verify_status_roundtrip() {
        let result = VerifyStatus::Failed {
            message: "cargo check failed".to_string(),
        };
        let json = serde_json::to_string(&result).unwrap();
        let decoded: VerifyStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, result);
    }

    #[test]
    fn stopped_state_is_terminal_and_not_active() {
        assert!(TaskState::Stopped.is_terminal());
        assert!(!TaskState::Stopped.is_active());
        assert!(!TaskState::Stopped.can_submit());
        assert!(!TaskState::Stopped.can_restack());
    }

    #[test]
    fn stopped_state_serializes_correctly() {
        let state = TaskState::Stopped;
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(json, "\"STOPPED\"");
        let decoded: TaskState = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, TaskState::Stopped);
    }

    #[test]
    fn stopped_state_display() {
        assert_eq!(format!("{}", TaskState::Stopped), "STOPPED");
    }

    #[test]
    fn can_submit_only_when_ready() {
        assert!(!TaskState::Chatting.can_submit());
        assert!(TaskState::Ready.can_submit());
        assert!(!TaskState::Submitting.can_submit());
        assert!(!TaskState::Merged.can_submit());
    }

    #[test]
    fn can_restack_in_correct_states() {
        assert!(!TaskState::Chatting.can_restack());
        assert!(TaskState::Ready.can_restack());
        assert!(TaskState::Submitting.can_restack());
        assert!(!TaskState::Restacking.can_restack());
        assert!(TaskState::AwaitingMerge.can_restack());
        assert!(!TaskState::Merged.can_restack());
        assert!(!TaskState::Stopped.can_restack());
    }

    #[test]
    fn is_active_only_when_chatting() {
        assert!(TaskState::Chatting.is_active());
        assert!(!TaskState::Ready.is_active());
        assert!(!TaskState::Submitting.is_active());
        assert!(!TaskState::Restacking.is_active());
        assert!(!TaskState::AwaitingMerge.is_active());
        assert!(!TaskState::Merged.is_active());
        assert!(!TaskState::Stopped.is_active());
    }

    #[test]
    fn is_terminal_only_for_merged_and_stopped() {
        assert!(!TaskState::Chatting.is_terminal());
        assert!(!TaskState::Ready.is_terminal());
        assert!(!TaskState::Submitting.is_terminal());
        assert!(!TaskState::Restacking.is_terminal());
        assert!(!TaskState::AwaitingMerge.is_terminal());
        assert!(TaskState::Merged.is_terminal());
        assert!(TaskState::Stopped.is_terminal());
    }

    #[test]
    fn verify_status_is_passed_and_is_failed() {
        assert!(!VerifyStatus::NotRun.is_passed());
        assert!(!VerifyStatus::NotRun.is_failed());

        assert!(!VerifyStatus::Running.is_passed());
        assert!(!VerifyStatus::Running.is_failed());

        assert!(VerifyStatus::Passed.is_passed());
        assert!(!VerifyStatus::Passed.is_failed());

        let failed = VerifyStatus::Failed {
            message: "error".to_string(),
        };
        assert!(!failed.is_passed());
        assert!(failed.is_failed());
    }

    #[test]
    fn verify_status_default_is_not_run() {
        let status = VerifyStatus::default();
        assert_eq!(status, VerifyStatus::NotRun);
    }

    #[test]
    fn task_state_display_all_variants() {
        assert_eq!(format!("{}", TaskState::Chatting), "CHATTING");
        assert_eq!(format!("{}", TaskState::Ready), "READY");
        assert_eq!(format!("{}", TaskState::Submitting), "SUBMITTING");
        assert_eq!(format!("{}", TaskState::Restacking), "RESTACKING");
        assert_eq!(format!("{}", TaskState::AwaitingMerge), "AWAITING_MERGE");
        assert_eq!(format!("{}", TaskState::Merged), "MERGED");
        assert_eq!(format!("{}", TaskState::Stopped), "STOPPED");
    }

    #[test]
    fn task_state_deserializes_from_screaming_snake_case() {
        let state: TaskState = serde_json::from_str("\"RESTACKING\"").unwrap();
        assert_eq!(state, TaskState::Restacking);

        let state: TaskState = serde_json::from_str("\"SUBMITTING\"").unwrap();
        assert_eq!(state, TaskState::Submitting);
    }

    #[test]
    fn verify_status_not_run_serialization() {
        let status = VerifyStatus::NotRun;
        let json = serde_json::to_string(&status).unwrap();
        let decoded: VerifyStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, VerifyStatus::NotRun);
    }

    #[test]
    fn verify_status_running_serialization() {
        let status = VerifyStatus::Running;
        let json = serde_json::to_string(&status).unwrap();
        let decoded: VerifyStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, VerifyStatus::Running);
    }

    #[test]
    fn verify_status_passed_serialization() {
        let status = VerifyStatus::Passed;
        let json = serde_json::to_string(&status).unwrap();
        let decoded: VerifyStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, VerifyStatus::Passed);
    }
}
