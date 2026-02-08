pub mod error;
pub mod mapper;
pub mod sink;
pub mod types;

pub use error::*;
pub use mapper::*;
pub use sink::*;
pub use types::*;

#[cfg(test)]
mod tests {
    use super::{
        notification_for_event, NotificationDispatcher, NotificationMessage, NotificationPolicy,
        NotificationSeverity, NotificationSinkKind, NotificationTopic, NotifyError, StdoutSink,
        TelegramSink,
    };
    use orch_core::events::Event;
    use std::any::TypeId;

    #[test]
    fn crate_root_reexports_types() {
        let _ = TypeId::of::<NotifyError>();
        let _ = TypeId::of::<NotificationMessage>();
        let _ = TypeId::of::<NotificationSeverity>();
        let _ = TypeId::of::<NotificationTopic>();
        let _ = TypeId::of::<NotificationSinkKind>();
        let _ = TypeId::of::<NotificationPolicy>();
        let _ = TypeId::of::<StdoutSink>();
        let _ = TypeId::of::<TelegramSink>();
        let _ = TypeId::of::<NotificationDispatcher>();
    }

    #[test]
    fn crate_root_reexports_mapper_helper() {
        let _mapper: fn(&Event) -> Option<NotificationMessage> = notification_for_event;
    }
}
