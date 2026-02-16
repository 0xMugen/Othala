use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

static ID_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub content: String,
    pub is_error: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub id: String,
    pub role: MessageRole,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub model: Option<String>,
    pub tokens: Option<u64>,
    pub tool_calls: Vec<ToolCall>,
    pub tool_results: Vec<ToolResult>,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub task_id: String,
    pub session_id: Option<String>,
    pub messages: Vec<ConversationMessage>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub total_tokens: u64,
    pub model_usage: HashMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConversationError {
    #[error("conversation not found: {0}")]
    NotFound(String),
    #[error("conversation already exists: {0}")]
    AlreadyExists(String),
    #[error("serialization error: {0}")]
    SerializationError(String),
    #[error("import error: {0}")]
    ImportError(String),
}

#[derive(Debug, Default)]
pub struct ConversationStore {
    pub conversations: HashMap<String, Conversation>,
    pub task_index: HashMap<String, Vec<String>>,
}

impl ConversationStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create_conversation(&mut self, task_id: &str, session_id: Option<&str>) -> String {
        let id = next_id("conv");
        let now = Utc::now();
        let conversation = Conversation {
            id: id.clone(),
            task_id: task_id.to_string(),
            session_id: session_id.map(ToOwned::to_owned),
            messages: Vec::new(),
            created_at: now,
            updated_at: now,
            total_tokens: 0,
            model_usage: HashMap::new(),
        };

        self.conversations.insert(id.clone(), conversation);
        self.task_index
            .entry(task_id.to_string())
            .or_default()
            .push(id.clone());
        id
    }

    pub fn add_message(
        &mut self,
        conversation_id: &str,
        message: ConversationMessage,
    ) -> Result<(), ConversationError> {
        let conversation = self
            .conversations
            .get_mut(conversation_id)
            .ok_or_else(|| ConversationError::NotFound(conversation_id.to_string()))?;

        if let Some(token_count) = message.tokens {
            conversation.total_tokens = conversation.total_tokens.saturating_add(token_count);
            if let Some(model) = &message.model {
                let entry = conversation.model_usage.entry(model.clone()).or_insert(0);
                *entry = entry.saturating_add(token_count);
            }
        }

        conversation.updated_at = Utc::now();
        conversation.messages.push(message);
        Ok(())
    }

    pub fn get_conversation(&self, id: &str) -> Option<&Conversation> {
        self.conversations.get(id)
    }

    pub fn get_task_conversations(&self, task_id: &str) -> Vec<&Conversation> {
        self.task_index
            .get(task_id)
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|id| self.conversations.get(id))
            .collect()
    }

    pub fn get_messages(
        &self,
        conversation_id: &str,
        limit: Option<usize>,
        offset: Option<usize>,
    ) -> Vec<&ConversationMessage> {
        let Some(conversation) = self.conversations.get(conversation_id) else {
            return Vec::new();
        };

        let start = offset.unwrap_or(0);
        if start >= conversation.messages.len() {
            return Vec::new();
        }

        let end = match limit {
            Some(value) => start.saturating_add(value).min(conversation.messages.len()),
            None => conversation.messages.len(),
        };

        conversation.messages[start..end].iter().collect()
    }

    pub fn get_last_n_messages(&self, conversation_id: &str, n: usize) -> Vec<&ConversationMessage> {
        let Some(conversation) = self.conversations.get(conversation_id) else {
            return Vec::new();
        };

        if n == 0 {
            return Vec::new();
        }

        let len = conversation.messages.len();
        let start = len.saturating_sub(n);
        conversation.messages[start..].iter().collect()
    }

    pub fn search_messages(&self, query: &str) -> Vec<(&Conversation, &ConversationMessage)> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }

        let needle = trimmed.to_lowercase();
        let mut matches = Vec::new();

        for conversation in self.conversations.values() {
            for message in &conversation.messages {
                if message.content.to_lowercase().contains(&needle) {
                    matches.push((conversation, message));
                }
            }
        }

        matches
    }

    pub fn total_tokens(&self, conversation_id: &str) -> u64 {
        self.conversations
            .get(conversation_id)
            .map(|conversation| conversation.total_tokens)
            .unwrap_or(0)
    }

    pub fn export_conversation(&self, id: &str) -> Result<String, ConversationError> {
        let conversation = self
            .conversations
            .get(id)
            .ok_or_else(|| ConversationError::NotFound(id.to_string()))?;

        serde_json::to_string_pretty(conversation)
            .map_err(|error| ConversationError::SerializationError(error.to_string()))
    }

    pub fn import_conversation(&mut self, json: &str) -> Result<String, ConversationError> {
        let conversation = serde_json::from_str::<Conversation>(json)
            .map_err(|error| ConversationError::ImportError(error.to_string()))?;

        if self.conversations.contains_key(&conversation.id) {
            return Err(ConversationError::AlreadyExists(conversation.id));
        }

        let id = conversation.id.clone();
        self.task_index
            .entry(conversation.task_id.clone())
            .or_default()
            .push(id.clone());
        self.conversations.insert(id.clone(), conversation);
        Ok(id)
    }

    pub fn delete_conversation(&mut self, id: &str) -> Result<(), ConversationError> {
        let Some(conversation) = self.conversations.remove(id) else {
            return Err(ConversationError::NotFound(id.to_string()));
        };

        if let Some(entries) = self.task_index.get_mut(&conversation.task_id) {
            entries.retain(|conversation_id| conversation_id != id);
            if entries.is_empty() {
                self.task_index.remove(&conversation.task_id);
            }
        }

        Ok(())
    }

    pub fn prune_old_conversations(&mut self, max_age_days: u64) -> usize {
        let Ok(days) = i64::try_from(max_age_days) else {
            return 0;
        };

        let cutoff = Utc::now() - Duration::days(days);
        let stale_ids: Vec<String> = self
            .conversations
            .iter()
            .filter_map(|(id, conversation)| {
                if conversation.updated_at < cutoff {
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect();

        let mut pruned = 0usize;
        for id in stale_ids {
            if self.delete_conversation(&id).is_ok() {
                pruned += 1;
            }
        }

        pruned
    }
}

fn next_id(prefix: &str) -> String {
    let sequence = ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp = Utc::now()
        .timestamp_nanos_opt()
        .unwrap_or_else(|| Utc::now().timestamp_millis().saturating_mul(1_000_000));
    let ts = u128::from(timestamp.unsigned_abs());
    format!(
        "{prefix}-{head:08x}-{mid:04x}-{tail:04x}",
        head = (ts & 0xffff_ffff),
        mid = ((ts >> 32) & 0xffff),
        tail = sequence & 0xffff,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use serde_json::json;

    fn mk_message(role: MessageRole, content: &str, tokens: Option<u64>) -> ConversationMessage {
        ConversationMessage {
            id: next_id("msg"),
            role,
            content: content.to_string(),
            timestamp: Utc::now(),
            model: Some("codex".to_string()),
            tokens,
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    fn mk_tool_call(name: &str) -> ToolCall {
        ToolCall {
            id: next_id("tool"),
            name: name.to_string(),
            arguments: json!({"path": "src/lib.rs"}),
            timestamp: Utc::now(),
        }
    }

    fn mk_tool_result(tool_call_id: &str, is_error: bool) -> ToolResult {
        ToolResult {
            tool_call_id: tool_call_id.to_string(),
            content: if is_error {
                "permission denied".to_string()
            } else {
                "ok".to_string()
            },
            is_error,
            duration_ms: 42,
        }
    }

    #[test]
    fn conversation_message_creation_supports_all_roles() {
        let system = mk_message(MessageRole::System, "You are helpful", None);
        let user = mk_message(MessageRole::User, "Please refactor", Some(13));
        let assistant = mk_message(MessageRole::Assistant, "Sure", Some(21));
        let tool = mk_message(MessageRole::Tool, "Tool output", None);

        assert_eq!(system.role, MessageRole::System);
        assert_eq!(user.role, MessageRole::User);
        assert_eq!(assistant.role, MessageRole::Assistant);
        assert_eq!(tool.role, MessageRole::Tool);
    }

    #[test]
    fn tool_call_creation_round_trip_fields() {
        let call = mk_tool_call("read_file");

        assert!(call.id.starts_with("tool-"));
        assert_eq!(call.name, "read_file");
        assert_eq!(call.arguments["path"], "src/lib.rs");
    }

    #[test]
    fn tool_result_creation_round_trip_fields() {
        let result = mk_tool_result("tool-1", true);

        assert_eq!(result.tool_call_id, "tool-1");
        assert!(result.is_error);
        assert_eq!(result.duration_ms, 42);
    }

    #[test]
    fn create_conversation_and_add_message_updates_tokens() {
        let mut store = ConversationStore::new();
        let conversation_id = store.create_conversation("T-100", Some("S-100"));

        let message = mk_message(MessageRole::Assistant, "Generated patch", Some(77));
        store
            .add_message(&conversation_id, message)
            .expect("add message should succeed");

        let conversation = store
            .get_conversation(&conversation_id)
            .expect("conversation exists");
        assert_eq!(conversation.task_id, "T-100");
        assert_eq!(conversation.session_id.as_deref(), Some("S-100"));
        assert_eq!(conversation.messages.len(), 1);
        assert_eq!(conversation.total_tokens, 77);
        assert_eq!(conversation.model_usage.get("codex"), Some(&77));
    }

    #[test]
    fn add_message_returns_not_found_for_unknown_conversation() {
        let mut store = ConversationStore::new();
        let result = store.add_message("missing", mk_message(MessageRole::User, "x", Some(1)));

        assert!(matches!(result, Err(ConversationError::NotFound(_))));
    }

    #[test]
    fn get_task_conversations_returns_only_matching_task() {
        let mut store = ConversationStore::new();
        let c1 = store.create_conversation("T-a", None);
        let c2 = store.create_conversation("T-a", None);
        let _ = store.create_conversation("T-b", None);

        let task_conversations = store.get_task_conversations("T-a");
        assert_eq!(task_conversations.len(), 2);
        assert_eq!(task_conversations[0].id, c1);
        assert_eq!(task_conversations[1].id, c2);
    }

    #[test]
    fn get_messages_respects_limit_and_offset() {
        let mut store = ConversationStore::new();
        let conversation_id = store.create_conversation("T-limit", None);
        for idx in 0..5 {
            store
                .add_message(
                    &conversation_id,
                    mk_message(MessageRole::User, &format!("message-{idx}"), Some(1)),
                )
                .expect("add message");
        }

        let page = store.get_messages(&conversation_id, Some(2), Some(1));
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].content, "message-1");
        assert_eq!(page[1].content, "message-2");
    }

    #[test]
    fn get_messages_handles_out_of_range_offset() {
        let mut store = ConversationStore::new();
        let conversation_id = store.create_conversation("T-offset", None);
        store
            .add_message(
                &conversation_id,
                mk_message(MessageRole::User, "single-message", Some(1)),
            )
            .expect("add message");

        let page = store.get_messages(&conversation_id, Some(3), Some(9));
        assert!(page.is_empty());
    }

    #[test]
    fn get_last_n_messages_returns_recent_items() {
        let mut store = ConversationStore::new();
        let conversation_id = store.create_conversation("T-last", None);
        for idx in 0..4 {
            store
                .add_message(
                    &conversation_id,
                    mk_message(MessageRole::Assistant, &format!("item-{idx}"), Some(2)),
                )
                .expect("add message");
        }

        let last = store.get_last_n_messages(&conversation_id, 2);
        assert_eq!(last.len(), 2);
        assert_eq!(last[0].content, "item-2");
        assert_eq!(last[1].content, "item-3");
    }

    #[test]
    fn search_messages_finds_matches_across_conversations() {
        let mut store = ConversationStore::new();
        let c1 = store.create_conversation("T-s1", None);
        let c2 = store.create_conversation("T-s2", None);

        store
            .add_message(
                &c1,
                mk_message(MessageRole::Assistant, "Refactor parser module", Some(11)),
            )
            .expect("add c1 message");
        store
            .add_message(&c2, mk_message(MessageRole::User, "parser crash", Some(4)))
            .expect("add c2 message");

        let matches = store.search_messages("parser");
        assert_eq!(matches.len(), 2);
    }

    #[test]
    fn search_messages_returns_empty_for_blank_query() {
        let mut store = ConversationStore::new();
        let c1 = store.create_conversation("T-empty", None);
        store
            .add_message(&c1, mk_message(MessageRole::User, "content", Some(1)))
            .expect("add message");

        let matches = store.search_messages("   ");
        assert!(matches.is_empty());
    }

    #[test]
    fn total_tokens_returns_zero_for_missing_conversation() {
        let store = ConversationStore::new();
        assert_eq!(store.total_tokens("missing"), 0);
    }

    #[test]
    fn export_and_import_round_trip_preserves_data() {
        let mut source = ConversationStore::new();
        let conversation_id = source.create_conversation("T-export", Some("S-export"));

        let mut message = mk_message(MessageRole::Assistant, "using read_file tool", Some(55));
        let call = mk_tool_call("read_file");
        let result = mk_tool_result(&call.id, false);
        message.tool_calls.push(call.clone());
        message.tool_results.push(result.clone());
        message
            .metadata
            .insert("phase".to_string(), "analysis".to_string());

        source
            .add_message(&conversation_id, message)
            .expect("add message");

        let exported = source
            .export_conversation(&conversation_id)
            .expect("export conversation");

        let mut target = ConversationStore::new();
        let imported_id = target
            .import_conversation(&exported)
            .expect("import conversation");

        assert_eq!(imported_id, conversation_id);
        let imported = target
            .get_conversation(&imported_id)
            .expect("imported conversation");
        assert_eq!(imported.session_id.as_deref(), Some("S-export"));
        assert_eq!(imported.messages.len(), 1);
        assert_eq!(imported.messages[0].tool_calls[0], call);
        assert_eq!(imported.messages[0].tool_results[0], result);
        assert_eq!(imported.messages[0].metadata.get("phase"), Some(&"analysis".to_string()));
    }

    #[test]
    fn import_conversation_rejects_duplicate_id() {
        let mut store = ConversationStore::new();
        let conversation_id = store.create_conversation("T-dupe", None);
        let json = store
            .export_conversation(&conversation_id)
            .expect("export conversation");

        let result = store.import_conversation(&json);
        assert!(matches!(result, Err(ConversationError::AlreadyExists(_))));
    }

    #[test]
    fn import_conversation_returns_import_error_for_invalid_json() {
        let mut store = ConversationStore::new();
        let result = store.import_conversation("not-json");
        assert!(matches!(result, Err(ConversationError::ImportError(_))));
    }

    #[test]
    fn delete_conversation_removes_from_store_and_task_index() {
        let mut store = ConversationStore::new();
        let id = store.create_conversation("T-delete", None);

        store
            .delete_conversation(&id)
            .expect("delete should succeed");

        assert!(store.get_conversation(&id).is_none());
        assert!(store.get_task_conversations("T-delete").is_empty());
    }

    #[test]
    fn delete_conversation_returns_error_when_missing() {
        let mut store = ConversationStore::new();
        let result = store.delete_conversation("missing");
        assert!(matches!(result, Err(ConversationError::NotFound(_))));
    }

    #[test]
    fn prune_old_conversations_deletes_stale_entries() {
        let mut store = ConversationStore::new();
        let old_id = store.create_conversation("T-old", None);
        let new_id = store.create_conversation("T-new", None);

        {
            let old = store
                .conversations
                .get_mut(&old_id)
                .expect("old conversation exists");
            old.updated_at = Utc::now() - Duration::days(40);
        }

        {
            let new = store
                .conversations
                .get_mut(&new_id)
                .expect("new conversation exists");
            new.updated_at = Utc::now() - Duration::days(1);
        }

        let removed = store.prune_old_conversations(30);
        assert_eq!(removed, 1);
        assert!(store.get_conversation(&old_id).is_none());
        assert!(store.get_conversation(&new_id).is_some());
    }

    #[test]
    fn prune_old_conversations_handles_large_age_without_panic() {
        let mut store = ConversationStore::new();
        let id = store.create_conversation("T-large", None);
        let removed = store.prune_old_conversations(u64::MAX);

        assert_eq!(removed, 0);
        assert!(store.get_conversation(&id).is_some());
    }

    #[test]
    fn conversation_error_display_impls_are_readable() {
        let not_found = ConversationError::NotFound("C-1".to_string()).to_string();
        let exists = ConversationError::AlreadyExists("C-2".to_string()).to_string();
        let serialization =
            ConversationError::SerializationError("bad json writer".to_string()).to_string();
        let import = ConversationError::ImportError("missing field".to_string()).to_string();

        assert!(not_found.contains("conversation not found"));
        assert!(exists.contains("already exists"));
        assert!(serialization.contains("serialization error"));
        assert!(import.contains("import error"));
    }
}
