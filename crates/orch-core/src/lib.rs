pub mod config;
pub mod events;
pub mod state;
pub mod types;
pub mod validation;

pub use config::*;
pub use events::*;
pub use state::*;
pub use types::*;
pub use validation::*;

#[cfg(test)]
mod tests {
    use super::{parse_org_config, ModelKind, TaskId, TaskState, Validate};
    use std::any::TypeId;

    #[test]
    fn crate_root_reexports_core_types() {
        let _ = TypeId::of::<TaskId>();
        let _ = TypeId::of::<TaskState>();
        let _ = TypeId::of::<ModelKind>();
    }

    #[test]
    fn crate_root_reexports_parse_and_validate_helpers() {
        let mut org = parse_org_config(
            r#"
[models]
enabled = ["claude", "codex"]
policy = "adaptive"
min_approvals = 2

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

        assert!(org.validate().is_empty());

        org.concurrency.per_repo = 0;
        let issues = org.validate();
        assert!(issues
            .iter()
            .any(|issue| issue.code == "concurrency.per_repo.zero"));
    }
}
