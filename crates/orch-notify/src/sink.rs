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
