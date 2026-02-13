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
}
