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
    VerifyFailed,
    RestackConflict,
    WaitingReviewCapacity,
    NeedsHuman,
    TaskError,
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
