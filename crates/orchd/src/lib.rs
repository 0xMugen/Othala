pub mod dependency_graph;
pub mod event_log;
pub mod lifecycle_gate;
pub mod persistence;
pub mod review_gate;
pub mod runtime;
pub mod scheduler;
pub mod service;
pub mod state_machine;
pub mod types;

pub use dependency_graph::*;
pub use event_log::*;
pub use lifecycle_gate::*;
pub use persistence::*;
pub use review_gate::*;
pub use runtime::*;
pub use scheduler::*;
pub use service::*;
pub use state_machine::*;
pub use types::*;

#[cfg(test)]
mod tests {
    use super::{
        is_transition_allowed, task_state_tag, JsonlEventLog, OrchdService, SchedulerConfig,
        SqliteStore, TaskRunRecord,
    };
    use orch_core::state::TaskState;

    #[test]
    fn crate_root_reexports_state_machine_helpers() {
        assert_eq!(task_state_tag(TaskState::DraftPrOpen), "DRAFT_PR_OPEN");
        assert!(is_transition_allowed(
            TaskState::Queued,
            TaskState::Initializing
        ));
    }

    #[test]
    fn crate_root_reexports_core_types() {
        let _ = std::any::TypeId::of::<SchedulerConfig>();
        let _ = std::any::TypeId::of::<SqliteStore>();
        let _ = std::any::TypeId::of::<JsonlEventLog>();
        let _ = std::any::TypeId::of::<OrchdService>();
        let _ = std::any::TypeId::of::<TaskRunRecord>();
    }
}
