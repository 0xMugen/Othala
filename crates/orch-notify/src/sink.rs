use crate::error::NotifyError;
use crate::types::{NotificationMessage, NotificationPolicy, NotificationSinkKind};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct RetryConfig {
    pub max_attempts: u32,
    pub initial_delay_ms: u64,
    pub backoff_multiplier: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_delay_ms: 1000,
            backoff_multiplier: 2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadLetterEntry {
    pub notification_id: String,
    pub sink_kind: String,
    pub payload: String,
    pub last_error: String,
    pub attempts: u32,
    pub created_at: String,
    pub last_attempt_at: String,
}

#[derive(Debug, Clone)]
pub struct DeadLetterQueue {
    entries: Vec<DeadLetterEntry>,
    max_size: usize,
}

impl DeadLetterQueue {
    pub fn new(max_size: usize) -> Self {
        Self {
            entries: Vec::new(),
            max_size,
        }
    }

    pub fn push(&mut self, entry: DeadLetterEntry) {
        if self.max_size == 0 {
            return;
        }

        if self.entries.len() >= self.max_size {
            self.entries.remove(0);
        }
        self.entries.push(entry);
    }

    pub fn entries(&self) -> &[DeadLetterEntry] {
        &self.entries
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for DeadLetterQueue {
    fn default() -> Self {
        Self::new(1000)
    }
}

pub fn exponential_backoff_delay_ms(config: &RetryConfig, attempt: u32) -> u64 {
    if attempt <= 1 {
        return config.initial_delay_ms;
    }

    let mut delay = config.initial_delay_ms;
    for _ in 1..attempt {
        delay = delay.saturating_mul(config.backoff_multiplier);
    }
    delay
}

fn should_retry(err: &NotifyError) -> bool {
    matches!(err, NotifyError::SinkFailed { .. })
}

fn build_dead_letter_entry(
    sink: &dyn NotificationSink,
    message: &NotificationMessage,
    attempts: u32,
    last_error: NotifyError,
) -> DeadLetterEntry {
    let now = Utc::now().to_rfc3339();
    let payload = serde_json::to_string(message).unwrap_or_else(|e| {
        format!(
            "{{\"error\":\"failed to serialize notification\",\"detail\":\"{}\"}}",
            e
        )
    });

    DeadLetterEntry {
        notification_id: format!(
            "{:?}-{}-{}",
            message.topic,
            message.at.timestamp_millis(),
            message.title
        ),
        sink_kind: format!("{:?}", sink.kind()),
        payload,
        last_error: last_error.to_string(),
        attempts,
        created_at: now.clone(),
        last_attempt_at: now,
    }
}

fn delivery_with_retry_sync(
    sink: &dyn NotificationSink,
    message: &NotificationMessage,
    config: &RetryConfig,
) -> Result<(), Box<DeadLetterEntry>> {
    let max_attempts = config.max_attempts.max(1);
    for attempt in 1..=max_attempts {
        match sink.send(message) {
            Ok(()) => return Ok(()),
            Err(err) => {
                if attempt >= max_attempts || !should_retry(&err) {
                    return Err(Box::new(build_dead_letter_entry(sink, message, attempt, err)));
                }

                let delay_ms = exponential_backoff_delay_ms(config, attempt);
                eprintln!(
                    "[notify] retry attempt {}/{} for {:?} in {}ms",
                    attempt + 1,
                    max_attempts,
                    sink.kind(),
                    delay_ms
                );

                if delay_ms > 0 {
                    thread::sleep(Duration::from_millis(delay_ms));
                }
            }
        }
    }

    Err(Box::new(build_dead_letter_entry(
        sink,
        message,
        max_attempts,
        NotifyError::SinkFailed {
            message: "retry loop exhausted without terminal result".to_string(),
        },
    )))
}

pub async fn delivery_with_retry(
    sink: &dyn NotificationSink,
    notification: &NotificationMessage,
    config: &RetryConfig,
) -> Result<(), Box<DeadLetterEntry>> {
    delivery_with_retry_sync(sink, notification, config)
}

pub trait NotificationSink: Send + Sync {
    fn kind(&self) -> NotificationSinkKind;
    fn send(&self, message: &NotificationMessage) -> Result<(), NotifyError>;
}

#[derive(Debug, Clone, Default)]
pub struct StdoutSink;

impl NotificationSink for StdoutSink {
    fn kind(&self) -> NotificationSinkKind {
        NotificationSinkKind::Stdout
    }

    fn send(&self, message: &NotificationMessage) -> Result<(), NotifyError> {
        println!(
            "[{:?}] {:?} {} | task={:?} repo={:?} | {}",
            message.severity,
            message.topic,
            message.title,
            message.task_id.as_ref().map(|x| x.0.clone()),
            message.repo_id.as_ref().map(|x| x.0.clone()),
            message.body
        );
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct TelegramSink {
    pub bot_token_env: String,
    pub chat_id_env: String,
    pub enabled: bool,
}

impl Default for TelegramSink {
    fn default() -> Self {
        Self {
            bot_token_env: "TELEGRAM_BOT_TOKEN".to_string(),
            chat_id_env: "TELEGRAM_CHAT_ID".to_string(),
            enabled: false,
        }
    }
}

impl NotificationSink for TelegramSink {
    fn kind(&self) -> NotificationSinkKind {
        NotificationSinkKind::Telegram
    }

    fn send(&self, _message: &NotificationMessage) -> Result<(), NotifyError> {
        if !self.enabled {
            return Err(NotifyError::SinkDisabled {
                sink: "telegram".to_string(),
            });
        }

        Err(NotifyError::SinkFailed {
            message: "telegram transport not implemented yet".to_string(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct WebhookSink {
    pub url: String,
    pub timeout_secs: u64,
}

impl NotificationSink for WebhookSink {
    fn kind(&self) -> NotificationSinkKind {
        NotificationSinkKind::Webhook
    }

    fn send(&self, message: &NotificationMessage) -> Result<(), NotifyError> {
        let payload = serde_json::json!({
            "topic": message.topic,
            "severity": message.severity,
            "title": &message.title,
            "body": &message.body,
            "task_id": message
                .task_id
                .as_ref()
                .map(|task_id| task_id.0.clone())
                .unwrap_or_default(),
        });
        let payload = serde_json::to_string(&payload).map_err(|e| NotifyError::SinkFailed {
            message: format!("failed to encode webhook payload: {e}"),
        })?;

        let output = Command::new("curl")
            .arg("-sS")
            .arg("-m")
            .arg(self.timeout_secs.to_string())
            .arg("-X")
            .arg("POST")
            .arg("-H")
            .arg("Content-Type: application/json")
            .arg("-d")
            .arg(payload)
            .arg(&self.url)
            .output()
            .map_err(|e| NotifyError::SinkFailed {
                message: format!("failed to execute curl for webhook sink: {e}"),
            })?;

        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(NotifyError::SinkFailed {
            message: format!(
                "webhook sink request failed (status {:?}): {}",
                output.status.code(),
                stderr.trim()
            ),
        })
    }
}

/// Slack webhook sink — posts notifications to a Slack channel via incoming webhook URL.
///
/// The webhook URL is expected to be set via the `SLACK_WEBHOOK_URL` environment variable
/// or configured directly. Messages are formatted as Slack Block Kit payloads.
#[derive(Debug, Clone)]
pub struct SlackSink {
    pub webhook_url: String,
    pub channel: Option<String>,
    pub timeout_secs: u64,
}

impl SlackSink {
    /// Build a Slack-formatted payload from a notification message.
    pub fn build_payload(
        message: &NotificationMessage,
        channel: Option<&str>,
    ) -> serde_json::Value {
        let severity_emoji = match message.severity {
            crate::types::NotificationSeverity::Info => "ℹ️",
            crate::types::NotificationSeverity::Warning => "⚠️",
            crate::types::NotificationSeverity::Error => "🔴",
        };

        let task_label = message
            .task_id
            .as_ref()
            .map(|t| format!(" | task: `{}`", t.0))
            .unwrap_or_default();

        let text = format!(
            "{} *{}*{}\n{}",
            severity_emoji, message.title, task_label, message.body
        );

        let mut payload = serde_json::json!({
            "text": text,
            "blocks": [
                {
                    "type": "section",
                    "text": {
                        "type": "mrkdwn",
                        "text": text
                    }
                }
            ]
        });

        if let Some(ch) = channel {
            payload["channel"] = serde_json::Value::String(ch.to_string());
        }

        payload
    }
}

impl NotificationSink for SlackSink {
    fn kind(&self) -> NotificationSinkKind {
        NotificationSinkKind::Slack
    }

    fn send(&self, message: &NotificationMessage) -> Result<(), NotifyError> {
        let payload = Self::build_payload(message, self.channel.as_deref());
        let payload_str =
            serde_json::to_string(&payload).map_err(|e| NotifyError::SinkFailed {
                message: format!("failed to encode Slack payload: {e}"),
            })?;

        let output = Command::new("curl")
            .arg("-sS")
            .arg("-m")
            .arg(self.timeout_secs.to_string())
            .arg("-X")
            .arg("POST")
            .arg("-H")
            .arg("Content-Type: application/json")
            .arg("-d")
            .arg(payload_str)
            .arg(&self.webhook_url)
            .output()
            .map_err(|e| NotifyError::SinkFailed {
                message: format!("failed to execute curl for Slack sink: {e}"),
            })?;

        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(NotifyError::SinkFailed {
            message: format!(
                "Slack webhook request failed (status {:?}): {}",
                output.status.code(),
                stderr.trim()
            ),
        })
    }
}

pub struct NotificationDispatcher {
    sinks: Vec<Box<dyn NotificationSink>>,
    retry_config: RetryConfig,
    dead_letters: Mutex<DeadLetterQueue>,
}

impl NotificationDispatcher {
    pub fn new(sinks: Vec<Box<dyn NotificationSink>>) -> Self {
        Self {
            sinks,
            retry_config: RetryConfig::default(),
            dead_letters: Mutex::new(DeadLetterQueue::default()),
        }
    }

    pub fn with_retry_config(sinks: Vec<Box<dyn NotificationSink>>, retry_config: RetryConfig) -> Self {
        Self {
            sinks,
            retry_config,
            dead_letters: Mutex::new(DeadLetterQueue::default()),
        }
    }

    pub fn with_retry_and_dead_letter_config(
        sinks: Vec<Box<dyn NotificationSink>>,
        retry_config: RetryConfig,
        dead_letter_max_size: usize,
    ) -> Self {
        Self {
            sinks,
            retry_config,
            dead_letters: Mutex::new(DeadLetterQueue::new(dead_letter_max_size)),
        }
    }

    pub fn from_policy(policy: &NotificationPolicy) -> Self {
        let mut sinks: Vec<Box<dyn NotificationSink>> = Vec::new();
        for sink in &policy.enabled_sinks {
            match sink {
                NotificationSinkKind::Stdout => sinks.push(Box::new(StdoutSink)),
                NotificationSinkKind::Telegram => sinks.push(Box::new(TelegramSink::default())),
                NotificationSinkKind::Webhook => {}
                NotificationSinkKind::Slack => {}
            }
        }
        Self {
            sinks,
            retry_config: RetryConfig::default(),
            dead_letters: Mutex::new(DeadLetterQueue::default()),
        }
    }

    pub fn failed_notifications(&self) -> Vec<DeadLetterEntry> {
        self.dead_letters
            .lock()
            .expect("dead letter queue lock")
            .entries()
            .to_vec()
    }

    pub fn clear_failed_notifications(&self) {
        self.dead_letters
            .lock()
            .expect("dead letter queue lock")
            .clear();
    }

    pub fn failed_notifications_len(&self) -> usize {
        self.dead_letters
            .lock()
            .expect("dead letter queue lock")
            .len()
    }

    pub fn dispatch(
        &self,
        message: &NotificationMessage,
    ) -> Vec<(NotificationSinkKind, Result<(), NotifyError>)> {
        let mut out = Vec::new();
        for sink in &self.sinks {
            let result = match delivery_with_retry_sync(sink.as_ref(), message, &self.retry_config) {
                Ok(()) => Ok(()),
                Err(dead_letter_entry) => {
                    let error_message = dead_letter_entry.last_error.clone();
                    self.dead_letters
                        .lock()
                        .expect("dead letter queue lock")
                        .push(*dead_letter_entry);

                    Err(NotifyError::SinkFailed {
                        message: format!(
                            "delivery failed after retry attempts: {}",
                            error_message
                        ),
                    })
                }
            };

            out.push((sink.kind(), result));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use orch_core::types::{RepoId, TaskId};
    use std::sync::{Arc, Mutex};

    use super::{
        exponential_backoff_delay_ms, DeadLetterEntry, DeadLetterQueue, NotificationDispatcher,
        NotificationSink, RetryConfig,
    };
    use crate::error::NotifyError;
    use crate::types::{
        NotificationMessage, NotificationPolicy, NotificationSeverity, NotificationSinkKind,
        NotificationTopic,
    };

    #[derive(Clone)]
    struct CaptureSink {
        kind: NotificationSinkKind,
        seen: Arc<Mutex<Vec<String>>>,
    }

    impl NotificationSink for CaptureSink {
        fn kind(&self) -> NotificationSinkKind {
            self.kind
        }

        fn send(&self, message: &NotificationMessage) -> Result<(), NotifyError> {
            self.seen
                .lock()
                .expect("capture lock")
                .push(message.title.clone());
            Ok(())
        }
    }

    #[derive(Clone)]
    struct AlwaysFailSink;

    impl NotificationSink for AlwaysFailSink {
        fn kind(&self) -> NotificationSinkKind {
            NotificationSinkKind::Telegram
        }

        fn send(&self, _message: &NotificationMessage) -> Result<(), NotifyError> {
            Err(NotifyError::SinkFailed {
                message: "fail".to_string(),
            })
        }
    }

    #[derive(Clone)]
    struct FlakySink {
        attempts: Arc<Mutex<u32>>,
        succeed_on: u32,
        kind: NotificationSinkKind,
    }

    impl NotificationSink for FlakySink {
        fn kind(&self) -> NotificationSinkKind {
            self.kind
        }

        fn send(&self, _message: &NotificationMessage) -> Result<(), NotifyError> {
            let mut attempts = self.attempts.lock().expect("flaky sink attempts lock");
            *attempts += 1;
            if *attempts >= self.succeed_on {
                Ok(())
            } else {
                Err(NotifyError::SinkFailed {
                    message: "transient failure".to_string(),
                })
            }
        }
    }

    fn mk_dead_letter(id: &str) -> DeadLetterEntry {
        DeadLetterEntry {
            notification_id: id.to_string(),
            sink_kind: "webhook".to_string(),
            payload: "{}".to_string(),
            last_error: "boom".to_string(),
            attempts: 3,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            last_attempt_at: "2026-01-01T00:00:01Z".to_string(),
        }
    }

    fn mk_message() -> NotificationMessage {
        NotificationMessage {
            at: Utc::now(),
            topic: NotificationTopic::VerifyFailed,
            severity: NotificationSeverity::Error,
            title: "verification failed".to_string(),
            body: "details".to_string(),
            task_id: Some(TaskId("T1".to_string())),
            repo_id: Some(RepoId("R1".to_string())),
        }
    }

    #[test]
    fn dispatch_fans_out_and_returns_per_sink_results() {
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let dispatcher = NotificationDispatcher::new(vec![
            Box::new(CaptureSink {
                kind: NotificationSinkKind::Stdout,
                seen: seen.clone(),
            }),
            Box::new(AlwaysFailSink),
        ]);

        let results = dispatcher.dispatch(&mk_message());
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, NotificationSinkKind::Stdout);
        assert!(results[0].1.is_ok());
        assert_eq!(results[1].0, NotificationSinkKind::Telegram);
        assert!(results[1].1.is_err());

        let captured = seen.lock().expect("capture lock");
        assert_eq!(captured.as_slice(), ["verification failed"]);
    }

    #[test]
    fn from_policy_builds_enabled_sinks() {
        let dispatcher = NotificationDispatcher::from_policy(&NotificationPolicy {
            enabled_sinks: vec![NotificationSinkKind::Stdout, NotificationSinkKind::Telegram],
        });
        let results = dispatcher.dispatch(&mk_message());
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, NotificationSinkKind::Stdout);
        assert!(results[0].1.is_ok());
        assert_eq!(results[1].0, NotificationSinkKind::Telegram);
        assert!(results[1].1.is_err());
    }

    #[test]
    fn from_policy_with_no_sinks_dispatches_to_none() {
        let dispatcher = NotificationDispatcher::from_policy(&NotificationPolicy {
            enabled_sinks: Vec::new(),
        });
        let results = dispatcher.dispatch(&mk_message());
        assert!(results.is_empty());
    }

    #[test]
    fn telegram_sink_returns_disabled_error_when_not_enabled() {
        let sink = super::TelegramSink::default();
        let err = sink
            .send(&mk_message())
            .expect_err("telegram default is disabled");
        assert!(matches!(
            err,
            NotifyError::SinkDisabled { sink } if sink == "telegram"
        ));
    }

    #[test]
    fn telegram_sink_returns_not_implemented_when_enabled() {
        let sink = super::TelegramSink {
            enabled: true,
            ..super::TelegramSink::default()
        };
        let err = sink
            .send(&mk_message())
            .expect_err("transport is not implemented");
        assert!(matches!(err, NotifyError::SinkFailed { .. }));
    }

    #[test]
    fn webhook_sink_kind_is_webhook() {
        let sink = super::WebhookSink {
            url: "https://example.test/webhook".to_string(),
            timeout_secs: 5,
        };
        assert_eq!(sink.kind(), NotificationSinkKind::Webhook);
    }

    #[test]
    fn dispatcher_with_stdout_sink_reports_success() {
        let dispatcher = NotificationDispatcher::new(vec![Box::new(super::StdoutSink)]);
        let results = dispatcher.dispatch(&mk_message());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, NotificationSinkKind::Stdout);
        assert!(results[0].1.is_ok());
    }

    #[test]
    fn slack_sink_kind_is_slack() {
        let sink = super::SlackSink {
            webhook_url: "https://hooks.slack.com/services/test".to_string(),
            channel: None,
            timeout_secs: 5,
        };
        assert_eq!(sink.kind(), NotificationSinkKind::Slack);
    }

    #[test]
    fn slack_payload_includes_severity_emoji_and_task_id() {
        let msg = mk_message();
        let payload = super::SlackSink::build_payload(&msg, None);
        let text = payload["text"].as_str().unwrap();
        assert!(text.contains("🔴"), "should contain error emoji");
        assert!(text.contains("T1"), "should contain task ID");
        assert!(text.contains("verification failed"), "should contain title");
    }

    #[test]
    fn slack_payload_includes_channel_when_set() {
        let msg = mk_message();
        let payload = super::SlackSink::build_payload(&msg, Some("#ops"));
        assert_eq!(payload["channel"].as_str().unwrap(), "#ops");
    }

    #[test]
    fn slack_payload_omits_channel_when_none() {
        let msg = mk_message();
        let payload = super::SlackSink::build_payload(&msg, None);
        assert!(payload.get("channel").is_none());
    }

    #[test]
    fn slack_payload_info_severity_uses_info_emoji() {
        let mut msg = mk_message();
        msg.severity = NotificationSeverity::Info;
        let payload = super::SlackSink::build_payload(&msg, None);
        let text = payload["text"].as_str().unwrap();
        assert!(text.contains("ℹ️"));
    }

    #[test]
    fn slack_payload_warning_severity_uses_warning_emoji() {
        let mut msg = mk_message();
        msg.severity = NotificationSeverity::Warning;
        let payload = super::SlackSink::build_payload(&msg, None);
        let text = payload["text"].as_str().unwrap();
        assert!(text.contains("⚠️"));
    }

    #[test]
    fn retry_succeeds_on_second_attempt() {
        let attempts = Arc::new(Mutex::new(0));
        let dispatcher = NotificationDispatcher::with_retry_config(
            vec![Box::new(FlakySink {
                attempts: attempts.clone(),
                succeed_on: 2,
                kind: NotificationSinkKind::Webhook,
            })],
            RetryConfig {
                max_attempts: 3,
                initial_delay_ms: 0,
                backoff_multiplier: 2,
            },
        );

        let results = dispatcher.dispatch(&mk_message());
        assert_eq!(results.len(), 1);
        assert!(results[0].1.is_ok());
        assert_eq!(*attempts.lock().expect("attempts lock"), 2);
        assert_eq!(dispatcher.failed_notifications_len(), 0);
    }

    #[test]
    fn retry_exhausts_attempts_to_dead_letter() {
        let dispatcher = NotificationDispatcher::with_retry_and_dead_letter_config(
            vec![Box::new(AlwaysFailSink)],
            RetryConfig {
                max_attempts: 3,
                initial_delay_ms: 0,
                backoff_multiplier: 2,
            },
            10,
        );

        let results = dispatcher.dispatch(&mk_message());
        assert_eq!(results.len(), 1);
        assert!(results[0].1.is_err());
        assert_eq!(dispatcher.failed_notifications_len(), 1);

        let failed = dispatcher.failed_notifications();
        assert_eq!(failed[0].attempts, 3);
        assert_eq!(failed[0].sink_kind, "Telegram");
    }

    #[test]
    fn dead_letter_queue_respects_max_size() {
        let mut queue = DeadLetterQueue::new(2);
        queue.push(mk_dead_letter("n1"));
        queue.push(mk_dead_letter("n2"));
        queue.push(mk_dead_letter("n3"));

        let entries = queue.entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].notification_id, "n2");
        assert_eq!(entries[1].notification_id, "n3");
    }

    #[test]
    fn dead_letter_queue_entries_returns_all() {
        let mut queue = DeadLetterQueue::new(5);
        queue.push(mk_dead_letter("a1"));
        queue.push(mk_dead_letter("a2"));

        let ids: Vec<&str> = queue
            .entries()
            .iter()
            .map(|entry| entry.notification_id.as_str())
            .collect();
        assert_eq!(ids, vec!["a1", "a2"]);
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn exponential_backoff_calculation() {
        let config = RetryConfig {
            max_attempts: 3,
            initial_delay_ms: 1000,
            backoff_multiplier: 2,
        };

        assert_eq!(exponential_backoff_delay_ms(&config, 1), 1000);
        assert_eq!(exponential_backoff_delay_ms(&config, 2), 2000);
        assert_eq!(exponential_backoff_delay_ms(&config, 3), 4000);
    }
}
