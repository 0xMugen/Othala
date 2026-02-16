use chrono::{DateTime, Utc};
use orch_core::types::{RepoId, TaskId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationTopic {
    TaskCreated,
    TaskCompleted,
    TaskCancelled,
    VerifyFailed,
    VerifyPassed,
    RestackConflict,
    WaitingReviewCapacity,
    NeedsHuman,
    TaskError,
    AgentSpawned,
    AgentCompleted,
    RetryScheduled,
    ConfigReloaded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationMessage {
    pub at: DateTime<Utc>,
    pub topic: NotificationTopic,
    pub severity: NotificationSeverity,
    pub title: String,
    pub body: String,
    pub task_id: Option<TaskId>,
    pub repo_id: Option<RepoId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationSinkKind {
    Stdout,
    Telegram,
    Webhook,
    Slack,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationPolicy {
    pub enabled_sinks: Vec<NotificationSinkKind>,
}

impl Default for NotificationPolicy {
    fn default() -> Self {
        Self {
            enabled_sinks: vec![NotificationSinkKind::Stdout],
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use orch_core::types::{RepoId, TaskId};

    use super::{
        NotificationMessage, NotificationPolicy, NotificationSeverity, NotificationSinkKind,
        NotificationTopic,
    };

    #[test]
    fn notification_policy_defaults_to_stdout_sink() {
        let policy = NotificationPolicy::default();
        assert_eq!(policy.enabled_sinks, vec![NotificationSinkKind::Stdout]);
    }

    #[test]
    fn enums_serialize_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&NotificationSeverity::Info).expect("serialize severity"),
            "\"info\""
        );
        assert_eq!(
            serde_json::to_string(&NotificationTopic::VerifyFailed).expect("serialize topic"),
            "\"verify_failed\""
        );
        assert_eq!(
            serde_json::to_string(&NotificationSinkKind::Stdout).expect("serialize sink kind"),
            "\"stdout\""
        );
        assert_eq!(
            serde_json::to_string(&NotificationSinkKind::Webhook).expect("serialize sink kind"),
            "\"webhook\""
        );
        assert_eq!(
            serde_json::to_string(&NotificationSinkKind::Slack).expect("serialize sink kind"),
            "\"slack\""
        );
        assert_eq!(
            serde_json::to_string(&NotificationTopic::AgentSpawned).expect("serialize topic"),
            "\"agent_spawned\""
        );
        assert_eq!(
            serde_json::to_string(&NotificationTopic::RetryScheduled).expect("serialize topic"),
            "\"retry_scheduled\""
        );
    }

    #[test]
    fn notification_message_roundtrip_preserves_optional_fields() {
        let message = NotificationMessage {
            at: Utc::now(),
            topic: NotificationTopic::TaskError,
            severity: NotificationSeverity::Error,
            title: "task failed".to_string(),
            body: "details".to_string(),
            task_id: Some(TaskId("T1".to_string())),
            repo_id: Some(RepoId("R1".to_string())),
        };

        let encoded = serde_json::to_string(&message).expect("serialize message");
        let decoded: NotificationMessage =
            serde_json::from_str(&encoded).expect("deserialize message");
        assert_eq!(decoded, message);
    }
}
