//! Othala daemon crate - MVP version.
//!
//! Next-gen orchestrator with multi-agent routing and smart context.

pub mod agent_dispatch;
pub mod agent_log;
pub mod attribution;
pub mod auto_compact;
pub mod chat_workspace;
pub mod ci_gen;
pub mod code_search;
pub mod context_manager;
pub mod conversation;
pub mod custom_commands;
pub mod context_gen;
pub mod context_graph;
pub mod daemon_loop;
pub mod daemon_status;
pub mod delegation;
pub mod dependency_graph;
pub mod e2e_tester;
pub mod graphite_agent;
pub mod orchestration_metrics;
pub mod problem_classifier;
pub mod sisyphus_recovery;
// prompt_builder intentionally not glob-reexported to avoid name collisions.
pub mod editor;
pub mod env_inject;
pub mod event_log;
pub mod file_watcher;
pub mod ignore;
pub mod lsp;
pub mod mcp;
pub mod mcp_resources;
pub mod mcp_transport;
pub mod metrics;
pub mod model_options;
pub mod persistence;
pub mod permissions;
pub mod prompt_builder;
pub mod prompt_mode;
pub mod provider_registry;
pub mod qa_agent;
pub mod qa_spec_gen;
pub mod rate_limiter;
pub mod retry;
pub mod scheduler;
pub mod search;
pub mod service;
pub mod shell_config;
pub mod stack_pipeline;
pub mod state_machine;
pub mod supervisor;
pub mod task_timeout;
pub mod task_templates;
pub mod test_spec;
pub mod types;
pub mod upgrade;
pub mod wizard;

pub use chat_workspace::*;
pub use context_graph::*;
pub use dependency_graph::*;
pub use event_log::*;
pub use persistence::*;
pub use permissions::*;
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
