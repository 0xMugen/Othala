//! Simplified chat state for MVP.
//!
//! This replaces the complex TaskState with a minimal set of states
//! focused on the core chat→submit→restack flow.

use serde::{Deserialize, Serialize};

/// MVP chat states - simplified from 16 to 6.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ChatState {
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
}

impl ChatState {
    /// Returns true if the chat is in a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(self, ChatState::Merged)
    }

    /// Returns true if the chat is actively working.
    pub fn is_active(&self) -> bool {
        matches!(self, ChatState::Chatting)
    }

    /// Returns true if the chat is ready to submit.
    pub fn can_submit(&self) -> bool {
        matches!(self, ChatState::Ready)
    }

    /// Returns true if the chat can be restacked.
    pub fn can_restack(&self) -> bool {
        matches!(
            self,
            ChatState::Ready | ChatState::Submitting | ChatState::AwaitingMerge
        )
    }
}

/// Simple verify result - pass or fail with optional message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyResult {
    NotRun,
    Running,
    Passed,
    Failed { message: String },
}

impl VerifyResult {
    pub fn is_passed(&self) -> bool {
        matches!(self, VerifyResult::Passed)
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, VerifyResult::Failed { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_state_serializes_as_screaming_snake_case() {
        let state = ChatState::Chatting;
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(json, "\"CHATTING\"");

        let state = ChatState::AwaitingMerge;
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(json, "\"AWAITING_MERGE\"");
    }

    #[test]
    fn terminal_state_check() {
        assert!(!ChatState::Chatting.is_terminal());
        assert!(!ChatState::Ready.is_terminal());
        assert!(ChatState::Merged.is_terminal());
    }

    #[test]
    fn verify_result_roundtrip() {
        let result = VerifyResult::Failed {
            message: "cargo check failed".to_string(),
        };
        let json = serde_json::to_string(&result).unwrap();
        let decoded: VerifyResult = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, result);
    }
}
