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

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use orch_core::events::EventKind;
    use orch_core::state::VerifyTier;
    use orch_core::types::{EventId, RepoId, TaskId};

    use super::notification_for_event;
    use crate::types::{NotificationSeverity, NotificationTopic};

    fn mk_event(kind: EventKind) -> orch_core::events::Event {
        orch_core::events::Event {
            id: EventId("E1".to_string()),
            task_id: Some(TaskId("T1".to_string())),
            repo_id: Some(RepoId("R1".to_string())),
            at: Utc::now(),
            kind,
        }
    }

    #[test]
    fn maps_failed_verify_to_error_notification() {
        let event = mk_event(EventKind::VerifyCompleted {
            tier: VerifyTier::Quick,
            success: false,
        });
        let message = notification_for_event(&event).expect("expected notification");
        assert_eq!(message.topic, NotificationTopic::VerifyFailed);
        assert_eq!(message.severity, NotificationSeverity::Error);
        assert_eq!(message.task_id, event.task_id);
        assert_eq!(message.repo_id, event.repo_id);
    }

    #[test]
    fn maps_empty_review_requested_to_waiting_capacity() {
        let event = mk_event(EventKind::ReviewRequested {
            required_models: Vec::new(),
        });
        let message = notification_for_event(&event).expect("expected notification");
        assert_eq!(message.topic, NotificationTopic::WaitingReviewCapacity);
        assert_eq!(message.severity, NotificationSeverity::Warning);
    }

    #[test]
    fn maps_needs_human_and_error_events() {
        let needs_human = mk_event(EventKind::NeedsHuman {
            reason: "manual decision required".to_string(),
        });
        let message = notification_for_event(&needs_human).expect("expected notification");
        assert_eq!(message.topic, NotificationTopic::NeedsHuman);
        assert!(message.body.contains("manual decision required"));

        let err = mk_event(EventKind::Error {
            code: "boom".to_string(),
            message: "exploded".to_string(),
        });
        let message = notification_for_event(&err).expect("expected notification");
        assert_eq!(message.topic, NotificationTopic::TaskError);
        assert_eq!(message.severity, NotificationSeverity::Error);
        assert!(message.title.contains("boom"));
        assert_eq!(message.body, "exploded");
    }

    #[test]
    fn ignores_non_notifying_events() {
        let event = mk_event(EventKind::TaskCreated);
        assert!(notification_for_event(&event).is_none());
    }
}
