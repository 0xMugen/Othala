//! Othala daemon crate - MVP version.

pub mod chat_workspace;
pub mod context_gen;
pub mod context_graph;
pub mod daemon_loop;
pub mod dependency_graph;
// prompt_builder intentionally not glob-reexported to avoid name collisions.
pub mod prompt_builder;
pub mod qa_agent;
pub mod qa_spec_gen;
pub mod retry;
pub mod stack_pipeline;
pub mod test_spec;
pub mod event_log;
pub mod persistence;
pub mod scheduler;
pub mod service;
pub mod state_machine;
pub mod supervisor;
pub mod types;

pub use chat_workspace::*;
pub use context_graph::*;
pub use dependency_graph::*;
pub use event_log::*;
pub use persistence::*;
pub use scheduler::*;
pub use service::*;
pub use state_machine::*;
pub use types::*;

#[cfg(test)]
mod tests {
    use super::{is_transition_allowed, task_state_tag};
    use orch_core::state::TaskState;

    #[test]
    fn crate_root_reexports_state_machine_helpers() {
        assert_eq!(task_state_tag(TaskState::Chatting), "CHATTING");
        assert!(is_transition_allowed(TaskState::Chatting, TaskState::Ready));
    }
}
