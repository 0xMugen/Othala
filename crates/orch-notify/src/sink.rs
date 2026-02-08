use crate::error::NotifyError;
use crate::types::{NotificationMessage, NotificationPolicy, NotificationSinkKind};

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

pub struct NotificationDispatcher {
    sinks: Vec<Box<dyn NotificationSink>>,
}

impl NotificationDispatcher {
    pub fn new(sinks: Vec<Box<dyn NotificationSink>>) -> Self {
        Self { sinks }
    }

    pub fn from_policy(policy: &NotificationPolicy) -> Self {
        let mut sinks: Vec<Box<dyn NotificationSink>> = Vec::new();
        for sink in &policy.enabled_sinks {
            match sink {
                NotificationSinkKind::Stdout => sinks.push(Box::new(StdoutSink)),
                NotificationSinkKind::Telegram => sinks.push(Box::new(TelegramSink::default())),
            }
        }
        Self { sinks }
    }

    pub fn dispatch(
        &self,
        message: &NotificationMessage,
    ) -> Vec<(NotificationSinkKind, Result<(), NotifyError>)> {
        let mut out = Vec::new();
        for sink in &self.sinks {
            out.push((sink.kind(), sink.send(message)));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use orch_core::types::{RepoId, TaskId};
    use std::sync::{Arc, Mutex};

    use super::{NotificationDispatcher, NotificationSink};
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
}
