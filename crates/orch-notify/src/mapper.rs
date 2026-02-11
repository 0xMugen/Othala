//! Map events to notifications for MVP.

use chrono::Utc;
use orch_core::events::{Event, EventKind};

use crate::types::{NotificationMessage, NotificationSeverity, NotificationTopic};

/// Map an event to a notification, if applicable.
pub fn notification_for_event(event: &Event) -> Option<NotificationMessage> {
    match &event.kind {
        EventKind::VerifyCompleted { success: false } => Some(NotificationMessage {
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
            body: "Restack conflict detected. Resolve conflicts manually.".to_string(),
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
    use orch_core::types::{EventId, RepoId, TaskId};

    use super::notification_for_event;
    use crate::types::{NotificationSeverity, NotificationTopic};

    fn mk_event(kind: EventKind) -> orch_core::events::Event {
        orch_core::events::Event {
            id: EventId("E1".to_string()),
            task_id: Some(TaskId::new("T1")),
            repo_id: Some(RepoId("R1".to_string())),
            at: Utc::now(),
            kind,
        }
    }

    #[test]
    fn maps_failed_verify_to_error_notification() {
        let event = mk_event(EventKind::VerifyCompleted { success: false });
        let message = notification_for_event(&event).expect("expected notification");
        assert_eq!(message.topic, NotificationTopic::VerifyFailed);
        assert_eq!(message.severity, NotificationSeverity::Error);
        assert_eq!(message.task_id, event.task_id);
        assert_eq!(message.repo_id, event.repo_id);
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
    fn maps_restack_conflict_to_warning_notification() {
        let event = mk_event(EventKind::RestackConflict);
        let message = notification_for_event(&event).expect("expected notification");
        assert_eq!(message.topic, NotificationTopic::RestackConflict);
        assert_eq!(message.severity, NotificationSeverity::Warning);
    }

    #[test]
    fn ignores_successful_verify() {
        let verify_ok = mk_event(EventKind::VerifyCompleted { success: true });
        assert!(notification_for_event(&verify_ok).is_none());
    }

    #[test]
    fn ignores_non_notifying_events() {
        let event = mk_event(EventKind::TaskCreated);
        assert!(notification_for_event(&event).is_none());
    }
}
