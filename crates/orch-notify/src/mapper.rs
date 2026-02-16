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
        EventKind::AgentCompleted {
            model,
            success: false,
            duration_secs,
        } => Some(NotificationMessage {
            at: Utc::now(),
            topic: NotificationTopic::TaskError,
            severity: NotificationSeverity::Error,
            title: format!("Agent failed ({model})"),
            body: format!("Agent run failed after {duration_secs}s."),
            task_id: event.task_id.clone(),
            repo_id: event.repo_id.clone(),
        }),
        EventKind::VerifyCompleted { success: true } => Some(NotificationMessage {
            at: Utc::now(),
            topic: NotificationTopic::VerifyPassed,
            severity: NotificationSeverity::Info,
            title: "Verification passed".to_string(),
            body: "All verification checks passed.".to_string(),
            task_id: event.task_id.clone(),
            repo_id: event.repo_id.clone(),
        }),
        EventKind::AgentSpawned { model } => Some(NotificationMessage {
            at: Utc::now(),
            topic: NotificationTopic::AgentSpawned,
            severity: NotificationSeverity::Info,
            title: format!("Agent spawned ({model})"),
            body: format!("Started agent run with {model}."),
            task_id: event.task_id.clone(),
            repo_id: event.repo_id.clone(),
        }),
        EventKind::AgentCompleted {
            model,
            success: true,
            duration_secs,
        } => Some(NotificationMessage {
            at: Utc::now(),
            topic: NotificationTopic::AgentCompleted,
            severity: NotificationSeverity::Info,
            title: format!("Agent completed ({model})"),
            body: format!("Agent run completed successfully in {duration_secs}s."),
            task_id: event.task_id.clone(),
            repo_id: event.repo_id.clone(),
        }),
        EventKind::ModelFallback {
            from_model,
            to_model,
            reason,
        } => Some(NotificationMessage {
            at: Utc::now(),
            topic: NotificationTopic::RetryScheduled,
            severity: NotificationSeverity::Warning,
            title: format!("Model fallback: {from_model} → {to_model}"),
            body: format!("Retrying with {to_model}: {reason}"),
            task_id: event.task_id.clone(),
            repo_id: event.repo_id.clone(),
        }),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationTemplate {
    pub topic: NotificationTopic,
    pub title_template: String,
    pub body_template: String,
}

#[derive(Debug, Clone)]
pub struct TemplateRegistry {
    templates: std::collections::HashMap<NotificationTopic, NotificationTemplate>,
}

impl Default for TemplateRegistry {
    fn default() -> Self {
        let mut registry = Self {
            templates: std::collections::HashMap::new(),
        };
        registry.register_defaults();
        registry
    }
}

impl TemplateRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn register_defaults(&mut self) {
        self.register(NotificationTemplate {
            topic: NotificationTopic::VerifyFailed,
            title_template: "Verification failed".to_string(),
            body_template: "Verification failed for task {{task_id}}.".to_string(),
        });
        self.register(NotificationTemplate {
            topic: NotificationTopic::VerifyPassed,
            title_template: "Verification passed".to_string(),
            body_template: "All checks passed for task {{task_id}}.".to_string(),
        });
        self.register(NotificationTemplate {
            topic: NotificationTopic::RestackConflict,
            title_template: "Restack conflict".to_string(),
            body_template: "Restack conflict on task {{task_id}}. Manual resolution needed."
                .to_string(),
        });
        self.register(NotificationTemplate {
            topic: NotificationTopic::NeedsHuman,
            title_template: "Task needs human input".to_string(),
            body_template: "Task {{task_id}} needs attention: {{reason}}".to_string(),
        });
        self.register(NotificationTemplate {
            topic: NotificationTopic::TaskError,
            title_template: "Error: {{code}}".to_string(),
            body_template: "Task {{task_id}} error: {{message}}".to_string(),
        });
        self.register(NotificationTemplate {
            topic: NotificationTopic::AgentSpawned,
            title_template: "Agent spawned ({{model}})".to_string(),
            body_template: "Started {{model}} agent for task {{task_id}}.".to_string(),
        });
        self.register(NotificationTemplate {
            topic: NotificationTopic::AgentCompleted,
            title_template: "Agent completed ({{model}})".to_string(),
            body_template: "{{model}} completed in {{duration}}s for task {{task_id}}.".to_string(),
        });
        self.register(NotificationTemplate {
            topic: NotificationTopic::RetryScheduled,
            title_template: "Retry: {{from_model}} → {{to_model}}".to_string(),
            body_template: "Falling back from {{from_model}} to {{to_model}}: {{reason}}"
                .to_string(),
        });
    }

    pub fn register(&mut self, template: NotificationTemplate) {
        self.templates.insert(template.topic, template);
    }

    pub fn get(&self, topic: &NotificationTopic) -> Option<&NotificationTemplate> {
        self.templates.get(topic)
    }

    pub fn len(&self) -> usize {
        self.templates.len()
    }

    pub fn is_empty(&self) -> bool {
        self.templates.is_empty()
    }
}

pub fn render_template(
    template: &str,
    vars: &std::collections::HashMap<String, String>,
) -> String {
    let mut result = template.to_string();
    for (key, value) in vars {
        result = result.replace(&format!("{{{{{key}}}}}"), value);
    }
    let re_start = "{{";
    let mut cleaned = String::with_capacity(result.len());
    let mut rest = result.as_str();
    while let Some(start) = rest.find(re_start) {
        cleaned.push_str(&rest[..start]);
        if let Some(end) = rest[start..].find("}}") {
            rest = &rest[start + end + 2..];
        } else {
            cleaned.push_str(&rest[start..]);
            rest = "";
            break;
        }
    }
    cleaned.push_str(rest);
    cleaned
}

pub fn vars_from_event(event: &Event) -> std::collections::HashMap<String, String> {
    let mut vars = std::collections::HashMap::new();

    if let Some(ref tid) = event.task_id {
        vars.insert("task_id".to_string(), tid.0.clone());
    }
    if let Some(ref rid) = event.repo_id {
        vars.insert("repo_id".to_string(), rid.0.clone());
    }

    match &event.kind {
        EventKind::NeedsHuman { reason } => {
            vars.insert("reason".to_string(), reason.clone());
        }
        EventKind::Error { code, message } => {
            vars.insert("code".to_string(), code.clone());
            vars.insert("message".to_string(), message.clone());
        }
        EventKind::AgentSpawned { model } => {
            vars.insert("model".to_string(), model.clone());
        }
        EventKind::AgentCompleted {
            model,
            success,
            duration_secs,
        } => {
            vars.insert("model".to_string(), model.clone());
            vars.insert("success".to_string(), success.to_string());
            vars.insert("duration".to_string(), duration_secs.to_string());
        }
        EventKind::ModelFallback {
            from_model,
            to_model,
            reason,
        } => {
            vars.insert("from_model".to_string(), from_model.clone());
            vars.insert("to_model".to_string(), to_model.clone());
            vars.insert("reason".to_string(), reason.clone());
        }
        _ => {}
    }

    vars
}

pub fn notification_from_template(
    event: &Event,
    registry: &TemplateRegistry,
) -> Option<NotificationMessage> {
    let base = notification_for_event(event)?;

    if let Some(template) = registry.get(&base.topic) {
        let vars = vars_from_event(event);
        Some(NotificationMessage {
            at: base.at,
            topic: base.topic,
            severity: base.severity,
            title: render_template(&template.title_template, &vars),
            body: render_template(&template.body_template, &vars),
            task_id: base.task_id,
            repo_id: base.repo_id,
        })
    } else {
        Some(base)
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
    fn maps_successful_verify_to_info() {
        let verify_ok = mk_event(EventKind::VerifyCompleted { success: true });
        let message = notification_for_event(&verify_ok).expect("expected notification");
        assert_eq!(message.topic, NotificationTopic::VerifyPassed);
        assert_eq!(message.severity, NotificationSeverity::Info);
    }

    #[test]
    fn ignores_non_notifying_events() {
        let event = mk_event(EventKind::TaskCreated);
        assert!(notification_for_event(&event).is_none());
    }

    #[test]
    fn maps_failed_agent_completion_to_error_notification() {
        let event = mk_event(EventKind::AgentCompleted {
            model: "claude".to_string(),
            success: false,
            duration_secs: 15,
        });

        let message = notification_for_event(&event).expect("expected notification");
        assert_eq!(message.topic, NotificationTopic::TaskError);
        assert_eq!(message.severity, NotificationSeverity::Error);
        assert!(message.title.contains("Agent failed"));
        assert!(message.body.contains("15s"));
    }

    #[test]
    fn returns_some_for_error_events() {
        let event = mk_event(EventKind::Error {
            code: "internal".to_string(),
            message: "unexpected failure".to_string(),
        });

        assert!(notification_for_event(&event).is_some());
    }

    #[test]
    fn maps_agent_spawned_to_info() {
        let event = mk_event(EventKind::AgentSpawned {
            model: "claude".to_string(),
        });
        let message = notification_for_event(&event).expect("expected notification");
        assert_eq!(message.topic, NotificationTopic::AgentSpawned);
        assert_eq!(message.severity, NotificationSeverity::Info);
        assert!(message.title.contains("claude"));
    }

    #[test]
    fn maps_successful_agent_completion_to_info() {
        let event = mk_event(EventKind::AgentCompleted {
            model: "codex".to_string(),
            success: true,
            duration_secs: 42,
        });
        let message = notification_for_event(&event).expect("expected notification");
        assert_eq!(message.topic, NotificationTopic::AgentCompleted);
        assert_eq!(message.severity, NotificationSeverity::Info);
        assert!(message.body.contains("42s"));
    }

    #[test]
    fn maps_model_fallback_to_retry_warning() {
        let event = mk_event(EventKind::ModelFallback {
            from_model: "claude".to_string(),
            to_model: "codex".to_string(),
            reason: "timeout".to_string(),
        });
        let message = notification_for_event(&event).expect("expected notification");
        assert_eq!(message.topic, NotificationTopic::RetryScheduled);
        assert_eq!(message.severity, NotificationSeverity::Warning);
        assert!(message.title.contains("claude"));
        assert!(message.title.contains("codex"));
    }

    #[test]
    fn render_template_substitutes_variables() {
        let mut vars = std::collections::HashMap::new();
        vars.insert("task_id".to_string(), "T42".to_string());
        vars.insert("model".to_string(), "claude".to_string());
        let result = super::render_template("Task {{task_id}} using {{model}}", &vars);
        assert_eq!(result, "Task T42 using claude");
    }

    #[test]
    fn render_template_strips_unresolved_placeholders() {
        let vars = std::collections::HashMap::new();
        let result = super::render_template("Hello {{name}}, status: ok", &vars);
        assert_eq!(result, "Hello , status: ok");
    }

    #[test]
    fn template_registry_has_defaults() {
        let registry = super::TemplateRegistry::new();
        assert!(!registry.is_empty());
        assert!(registry.get(&NotificationTopic::VerifyFailed).is_some());
        assert!(registry.get(&NotificationTopic::AgentSpawned).is_some());
        assert!(registry.get(&NotificationTopic::RetryScheduled).is_some());
    }

    #[test]
    fn template_registry_custom_override() {
        let mut registry = super::TemplateRegistry::new();
        registry.register(super::NotificationTemplate {
            topic: NotificationTopic::VerifyFailed,
            title_template: "CUSTOM: verify failed".to_string(),
            body_template: "Custom body for {{task_id}}".to_string(),
        });
        let tmpl = registry.get(&NotificationTopic::VerifyFailed).unwrap();
        assert_eq!(tmpl.title_template, "CUSTOM: verify failed");
    }

    #[test]
    fn notification_from_template_uses_registry() {
        let registry = super::TemplateRegistry::new();
        let event = mk_event(EventKind::AgentSpawned {
            model: "gemini".to_string(),
        });
        let msg = super::notification_from_template(&event, &registry).unwrap();
        assert!(msg.title.contains("gemini"));
        assert!(msg.body.contains("T1"));
    }

    #[test]
    fn vars_from_event_extracts_agent_fields() {
        let event = mk_event(EventKind::AgentCompleted {
            model: "codex".to_string(),
            success: true,
            duration_secs: 99,
        });
        let vars = super::vars_from_event(&event);
        assert_eq!(vars.get("model").unwrap(), "codex");
        assert_eq!(vars.get("duration").unwrap(), "99");
        assert_eq!(vars.get("task_id").unwrap(), "T1");
    }
}
