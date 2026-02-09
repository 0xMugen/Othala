//! Chat entity for MVP.
//!
//! A Chat represents an AI coding session that produces code changes
//! and auto-submits to Graphite when complete.

use crate::chat_state::{ChatState, VerifyResult};
use crate::types::ModelKind;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Unique identifier for a chat.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChatId(pub String);

impl ChatId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ChatId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A chat (AI coding session) that produces code and auto-submits to Graphite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chat {
    /// Unique identifier
    pub id: ChatId,

    /// Repository this chat operates on
    pub repo_id: String,

    /// Human-readable title
    pub title: String,

    /// Git branch for this chat's work
    pub branch: String,

    /// AI model powering this chat
    pub model: ModelKind,

    /// Current state
    pub state: ChatState,

    /// Verification result
    pub verify: VerifyResult,

    /// Explicit dependencies - this chat waits for these to merge
    pub depends_on: Vec<ChatId>,

    /// Implicit parent - this chat stacks on top of this one
    /// When parent merges, this chat auto-restacks onto main
    pub parent_chat: Option<ChatId>,

    /// Graphite PR URL once submitted
    pub pr_url: Option<String>,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,

    /// Completion timestamp (when state became Ready)
    pub completed_at: Option<DateTime<Utc>>,

    /// Merge timestamp
    pub merged_at: Option<DateTime<Utc>>,
}

impl Chat {
    /// Create a new chat in Chatting state.
    pub fn new(
        id: ChatId,
        repo_id: String,
        title: String,
        branch: String,
        model: ModelKind,
    ) -> Self {
        Self {
            id,
            repo_id,
            title,
            branch,
            model,
            state: ChatState::Chatting,
            verify: VerifyResult::NotRun,
            depends_on: Vec::new(),
            parent_chat: None,
            pr_url: None,
            created_at: Utc::now(),
            completed_at: None,
            merged_at: None,
        }
    }

    /// Stack this chat on top of a parent chat.
    pub fn with_parent(mut self, parent: ChatId) -> Self {
        self.parent_chat = Some(parent);
        self
    }

    /// Add explicit dependency.
    pub fn depends_on(mut self, dep: ChatId) -> Self {
        self.depends_on.push(dep);
        self
    }

    /// Check if all explicit dependencies are resolved (merged).
    pub fn dependencies_resolved(&self, chats: &[Chat]) -> bool {
        self.depends_on.iter().all(|dep_id| {
            chats
                .iter()
                .find(|c| &c.id == dep_id)
                .map(|c| c.state == ChatState::Merged)
                .unwrap_or(false)
        })
    }

    /// Check if parent needs restacking (parent merged but we haven't restacked).
    pub fn needs_restack(&self, chats: &[Chat]) -> bool {
        if let Some(ref parent_id) = self.parent_chat {
            chats
                .iter()
                .find(|c| &c.id == parent_id)
                .map(|c| c.state == ChatState::Merged && self.state != ChatState::Merged)
                .unwrap_or(false)
        } else {
            false
        }
    }

    /// Transition to Ready state.
    pub fn mark_ready(&mut self) {
        self.state = ChatState::Ready;
        self.completed_at = Some(Utc::now());
    }

    /// Transition to Submitting state.
    pub fn mark_submitting(&mut self) {
        self.state = ChatState::Submitting;
    }

    /// Transition to Restacking state.
    pub fn mark_restacking(&mut self) {
        self.state = ChatState::Restacking;
    }

    /// Transition to AwaitingMerge state with PR URL.
    pub fn mark_submitted(&mut self, pr_url: String) {
        self.state = ChatState::AwaitingMerge;
        self.pr_url = Some(pr_url);
    }

    /// Transition to Merged state.
    pub fn mark_merged(&mut self) {
        self.state = ChatState::Merged;
        self.merged_at = Some(Utc::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_chat(id: &str, state: ChatState) -> Chat {
        let mut chat = Chat::new(
            ChatId::new(id),
            "test-repo".to_string(),
            format!("Test {}", id),
            format!("chat/{}", id),
            ModelKind::Claude,
        );
        chat.state = state;
        chat
    }

    #[test]
    fn new_chat_starts_in_chatting_state() {
        let chat = Chat::new(
            ChatId::new("C1"),
            "repo".to_string(),
            "Test".to_string(),
            "branch".to_string(),
            ModelKind::Claude,
        );
        assert_eq!(chat.state, ChatState::Chatting);
    }

    #[test]
    fn dependencies_resolved_when_all_merged() {
        let c1 = make_chat("C1", ChatState::Merged);
        let c2 = make_chat("C2", ChatState::Merged);
        let c3 = make_chat("C3", ChatState::Chatting)
            .depends_on(ChatId::new("C1"))
            .depends_on(ChatId::new("C2"));

        let chats = vec![c1, c2, c3.clone()];
        assert!(c3.dependencies_resolved(&chats));
    }

    #[test]
    fn dependencies_not_resolved_when_some_not_merged() {
        let c1 = make_chat("C1", ChatState::Merged);
        let c2 = make_chat("C2", ChatState::Ready); // Not merged!
        let c3 = make_chat("C3", ChatState::Chatting)
            .depends_on(ChatId::new("C1"))
            .depends_on(ChatId::new("C2"));

        let chats = vec![c1, c2, c3.clone()];
        assert!(!c3.dependencies_resolved(&chats));
    }

    #[test]
    fn needs_restack_when_parent_merged() {
        let parent = make_chat("P1", ChatState::Merged);
        let child = make_chat("C1", ChatState::Ready).with_parent(ChatId::new("P1"));

        let chats = vec![parent, child.clone()];
        assert!(child.needs_restack(&chats));
    }
}
