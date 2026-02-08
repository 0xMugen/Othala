pub mod client;
pub mod command;
pub mod error;
pub mod types;

pub use client::*;
pub use command::*;
pub use error::*;
pub use types::*;

#[cfg(test)]
mod tests {
    use super::{
        infer_task_dependencies_from_stack, looks_like_restack_conflict, parse_gt_log_short,
        AllowedAutoCommand, GraphiteCli, GraphiteClient, GraphiteError, GraphiteStackSnapshot,
        GraphiteStatusSnapshot, InferredStackDependency, RestackOutcome, StackNode,
    };
    use std::any::TypeId;
    use std::path::{Path, PathBuf};

    #[test]
    fn crate_root_reexports_types() {
        let _ = TypeId::of::<AllowedAutoCommand>();
        let _ = TypeId::of::<GraphiteCli>();
        let _ = TypeId::of::<GraphiteClient>();
        let _ = TypeId::of::<GraphiteError>();
        let _ = TypeId::of::<GraphiteStatusSnapshot>();
        let _ = TypeId::of::<GraphiteStackSnapshot>();
        let _ = TypeId::of::<StackNode>();
        let _ = TypeId::of::<InferredStackDependency>();
        let _ = TypeId::of::<RestackOutcome>();
    }

    #[test]
    fn crate_root_reexports_helpers_and_methods() {
        let _parse: fn(&str) -> GraphiteStackSnapshot = parse_gt_log_short;
        let _infer: fn(
            &GraphiteStackSnapshot,
            &std::collections::HashMap<String, orch_core::types::TaskId>,
        ) -> Vec<InferredStackDependency> = infer_task_dependencies_from_stack;
        let _conflict: fn(&str, &str) -> bool = looks_like_restack_conflict;

        let client = GraphiteClient::new(PathBuf::from("/tmp/repo"));
        let root: &Path = client.repo_root();
        assert_eq!(root, Path::new("/tmp/repo"));
    }
}
