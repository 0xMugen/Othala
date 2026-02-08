use chrono::Utc;
use orch_core::events::{Event, EventKind};

use crate::types::{NotificationMessage, NotificationSeverity, NotificationTopic};

pub fn notification_for_event(event: &Event) -> Option<NotificationMessage> {
    match &event.kind {
        EventKind::VerifyCompleted {
            tier: _,
            success: false,
        } => Some(NotificationMessage {
            at: Utc::now(),
            topic: NotificationTopic::VerifyFailed,
            severity: NotificationSeverity::Error,
            title: "Verification failed".to_string(),
            body: "A verification command failed. Check verify logs for details.".to_string(),
            task_id: event.task_id.clone(),
            repo_id: event.repo_id.clone(),
        }),
        EventKind::RestackConflict => Some(NotificationMessage {
            at: Utc::now(),
            topic: NotificationTopic::RestackConflict,
            severity: NotificationSeverity::Warning,
            title: "Restack conflict".to_string(),
            body: "Restack conflict detected. Resolve conflicts, then run `gt add -A` and `gt continue`."
                .to_string(),
            task_id: event.task_id.clone(),
            repo_id: event.repo_id.clone(),
        }),
        EventKind::NeedsHuman { reason } => Some(NotificationMessage {
            at: Utc::now(),
            topic: NotificationTopic::NeedsHuman,
            severity: NotificationSeverity::Warning,
            title: "Task needs human input".to_string(),
            body: format!("Task marked NEEDS_HUMAN: {reason}"),
            task_id: event.task_id.clone(),
            repo_id: event.repo_id.clone(),
        }),
        EventKind::ReviewRequested { required_models } if required_models.is_empty() => {
            Some(NotificationMessage {
                at: Utc::now(),
                topic: NotificationTopic::WaitingReviewCapacity,
                severity: NotificationSeverity::Warning,
                title: "Waiting for review capacity".to_string(),
                body: "No reviewers available for this task based on current policy/capacity."
                    .to_string(),
                task_id: event.task_id.clone(),
                repo_id: event.repo_id.clone(),
            })
        }
        EventKind::Error { code, message } => Some(NotificationMessage {
            at: Utc::now(),
            topic: NotificationTopic::TaskError,
            severity: NotificationSeverity::Error,
            title: format!("Task error: {code}"),
            body: message.clone(),
            task_id: event.task_id.clone(),
            repo_id: event.repo_id.clone(),
        }),
        _ => None,
    }
}
