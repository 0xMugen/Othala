//! Core types for the Othala MVP orchestrator.

pub mod config;
pub mod events;
pub mod state;
pub mod types;
pub mod validation;

// Re-export core types for convenience
pub use config::*;
pub use events::*;
pub use state::*;
pub use types::*;
pub use validation::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::TypeId;

    #[test]
    fn crate_root_reexports_core_types() {
        let _ = TypeId::of::<TaskId>();
        let _ = TypeId::of::<TaskState>();
        let _ = TypeId::of::<ModelKind>();
    }

    #[test]
    fn crate_root_reexports_parse_helpers() {
        let org = parse_org_config(
            r#"
[models]
enabled = ["claude", "codex"]

[concurrency]
per_repo = 10
claude = 10
codex = 10
gemini = 10

[graphite]
auto_submit = true
submit_mode_default = "single"
allow_move = "manual"

[ui]
web_bind = "127.0.0.1:9842"
"#,
        )
        .expect("parse org");

        assert_eq!(org.models.enabled.len(), 2);
    }
}
