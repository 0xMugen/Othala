//! Auto-compact -- monitors token usage and triggers context summarization.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Configuration for auto-compact behavior
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoCompactConfig {
    /// Whether auto-compact is enabled
    pub enabled: bool,
    /// Trigger compaction at this percentage of context window (0.0-1.0)
    pub threshold: f64,
    /// Maximum context window size in tokens (per model)
    pub context_window_sizes: HashMap<String, u64>,
    /// Minimum number of messages before compaction is allowed
    pub min_messages_before_compact: usize,
}

impl Default for AutoCompactConfig {
    fn default() -> Self {
        let mut sizes = HashMap::new();
        sizes.insert("claude".to_string(), 200_000);
        sizes.insert("codex".to_string(), 128_000);
        sizes.insert("gemini".to_string(), 1_000_000);

        Self {
            enabled: true,
            threshold: 0.95,
            context_window_sizes: sizes,
            min_messages_before_compact: 5,
        }
    }
}

/// Tracks token usage for a conversation
#[derive(Debug, Clone, Default)]
pub struct TokenTracker {
    /// Total tokens used in current context
    pub total_tokens: u64,
    /// Input tokens
    pub input_tokens: u64,
    /// Output tokens
    pub output_tokens: u64,
    /// Number of messages in conversation
    pub message_count: usize,
    /// History of compaction events
    pub compaction_history: Vec<CompactionEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionEvent {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub tokens_before: u64,
    pub tokens_after: u64,
    pub messages_compacted: usize,
    pub summary_length: usize,
}

impl TokenTracker {
    pub fn new() -> Self {
        Default::default()
    }

    /// Record token usage from a message
    pub fn record_usage(&mut self, input: u64, output: u64) {
        self.input_tokens += input;
        self.output_tokens += output;
        self.total_tokens = self.input_tokens + self.output_tokens;
        self.message_count += 1;
    }

    /// Check if compaction should trigger
    pub fn should_compact(&self, config: &AutoCompactConfig, model: &str) -> bool {
        if !config.enabled {
            return false;
        }
        if self.message_count < config.min_messages_before_compact {
            return false;
        }
        let window = config
            .context_window_sizes
            .get(model)
            .copied()
            .unwrap_or(128_000);
        let threshold = (window as f64 * config.threshold) as u64;
        self.total_tokens >= threshold
    }

    /// Record that compaction occurred
    pub fn record_compaction(
        &mut self,
        tokens_after: u64,
        messages_compacted: usize,
        summary_length: usize,
    ) {
        let event = CompactionEvent {
            timestamp: chrono::Utc::now(),
            tokens_before: self.total_tokens,
            tokens_after,
            messages_compacted,
            summary_length,
        };
        self.compaction_history.push(event);
        self.total_tokens = tokens_after;
        self.input_tokens = tokens_after;
        self.output_tokens = 0;
        self.message_count = 1; // summary counts as 1 message
    }

    /// Get context utilization as percentage
    pub fn utilization(&self, config: &AutoCompactConfig, model: &str) -> f64 {
        let window = config
            .context_window_sizes
            .get(model)
            .copied()
            .unwrap_or(128_000);
        if window == 0 {
            return 0.0;
        }
        self.total_tokens as f64 / window as f64
    }

    /// Get human-readable status
    pub fn status_line(&self, config: &AutoCompactConfig, model: &str) -> String {
        let util = self.utilization(config, model) * 100.0;
        let window = config
            .context_window_sizes
            .get(model)
            .copied()
            .unwrap_or(128_000);
        format!(
            "{}/{} tokens ({:.1}%), {} messages, {} compactions",
            self.total_tokens,
            window,
            util,
            self.message_count,
            self.compaction_history.len()
        )
    }
}

/// Build a compaction summary prompt
pub fn build_compaction_prompt(messages: &[String]) -> String {
    let mut prompt = String::from(
        "Summarize the following conversation concisely, preserving:\n\
         - Key decisions made\n\
         - Current task state and progress\n\
         - Important code changes or findings\n\
         - Any unresolved issues\n\n\
         Conversation:\n",
    );
    for (i, msg) in messages.iter().enumerate() {
        prompt.push_str(&format!("[Message {}] {}\n", i + 1, msg));
    }
    prompt
}

/// Estimate tokens from text (rough: ~4 chars per token)
pub fn estimate_tokens(text: &str) -> u64 {
    (text.len() as u64).div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::{build_compaction_prompt, estimate_tokens, AutoCompactConfig, TokenTracker};

    #[test]
    fn default_config_values() {
        let config = AutoCompactConfig::default();
        assert!(config.enabled);
        assert_eq!(config.threshold, 0.95);
        assert_eq!(config.min_messages_before_compact, 5);
        assert_eq!(config.context_window_sizes.get("claude"), Some(&200_000));
        assert_eq!(config.context_window_sizes.get("codex"), Some(&128_000));
        assert_eq!(config.context_window_sizes.get("gemini"), Some(&1_000_000));
    }

    #[test]
    fn token_tracking_record_usage() {
        let mut tracker = TokenTracker::new();
        tracker.record_usage(120, 30);
        tracker.record_usage(20, 10);

        assert_eq!(tracker.input_tokens, 140);
        assert_eq!(tracker.output_tokens, 40);
        assert_eq!(tracker.total_tokens, 180);
        assert_eq!(tracker.message_count, 2);
    }

    #[test]
    fn should_compact_below_threshold() {
        let mut tracker = TokenTracker::new();
        let config = AutoCompactConfig::default();

        for _ in 0..5 {
            tracker.record_usage(20_000, 0);
        }

        assert!(!tracker.should_compact(&config, "codex"));
    }

    #[test]
    fn should_compact_above_threshold() {
        let mut tracker = TokenTracker::new();
        let config = AutoCompactConfig::default();

        for _ in 0..5 {
            tracker.record_usage(25_000, 0);
        }

        assert!(tracker.should_compact(&config, "codex"));
    }

    #[test]
    fn should_compact_disabled() {
        let mut tracker = TokenTracker::new();
        let mut config = AutoCompactConfig::default();
        config.enabled = false;

        for _ in 0..8 {
            tracker.record_usage(30_000, 0);
        }

        assert!(!tracker.should_compact(&config, "codex"));
    }

    #[test]
    fn should_compact_not_enough_messages() {
        let mut tracker = TokenTracker::new();
        let config = AutoCompactConfig::default();

        for _ in 0..4 {
            tracker.record_usage(40_000, 0);
        }

        assert!(!tracker.should_compact(&config, "codex"));
    }

    #[test]
    fn record_compaction_resets_counters() {
        let mut tracker = TokenTracker::new();
        tracker.record_usage(1_000, 250);
        tracker.record_compaction(300, 4, 80);

        assert_eq!(tracker.total_tokens, 300);
        assert_eq!(tracker.input_tokens, 300);
        assert_eq!(tracker.output_tokens, 0);
        assert_eq!(tracker.message_count, 1);
        assert_eq!(tracker.compaction_history.len(), 1);
        assert_eq!(tracker.compaction_history[0].tokens_before, 1_250);
        assert_eq!(tracker.compaction_history[0].tokens_after, 300);
        assert_eq!(tracker.compaction_history[0].messages_compacted, 4);
        assert_eq!(tracker.compaction_history[0].summary_length, 80);
    }

    #[test]
    fn utilization_calculation() {
        let mut tracker = TokenTracker::new();
        let config = AutoCompactConfig::default();
        tracker.record_usage(64_000, 0);

        let util = tracker.utilization(&config, "codex");
        assert!((util - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn status_line_format() {
        let mut tracker = TokenTracker::new();
        let config = AutoCompactConfig::default();
        tracker.record_usage(64_000, 0);

        let status = tracker.status_line(&config, "codex");
        assert!(status.contains("64000/128000 tokens (50.0%)"));
        assert!(status.contains("1 messages"));
        assert!(status.contains("0 compactions"));
    }

    #[test]
    fn estimate_tokens_rough_accuracy() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("a"), 1);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
    }

    #[test]
    fn build_compaction_prompt_format() {
        let messages = vec!["hello".to_string(), "world".to_string()];
        let prompt = build_compaction_prompt(&messages);

        assert!(prompt.contains("Summarize the following conversation concisely"));
        assert!(prompt.contains("[Message 1] hello"));
        assert!(prompt.contains("[Message 2] world"));
    }
}
