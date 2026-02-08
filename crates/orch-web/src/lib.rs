pub mod error;
pub mod merge_queue;
pub mod model;
pub mod routes;
pub mod sandbox;
pub mod server;
pub mod state;

pub use error::*;
pub use merge_queue::*;
pub use model::*;
pub use routes::*;
pub use sandbox::*;
pub use server::*;
pub use state::*;

#[cfg(test)]
mod tests {
    use super::{
        build_merge_queue, router, run_web_server, spawn_sandbox_run, web_event_name, ErrorBody,
        MergeQueueResponse, SandboxRunView, SandboxSpawnRequest, SandboxSpawnResponse, WebError,
        WebEvent, WebEventKind, WebState,
    };
    use std::any::TypeId;

    #[test]
    fn crate_root_reexports_types() {
        let _ = TypeId::of::<WebError>();
        let _ = TypeId::of::<ErrorBody>();
        let _ = TypeId::of::<WebState>();
        let _ = TypeId::of::<MergeQueueResponse>();
        let _ = TypeId::of::<SandboxSpawnRequest>();
        let _ = TypeId::of::<SandboxSpawnResponse>();
        let _ = TypeId::of::<SandboxRunView>();
        let _ = TypeId::of::<WebEvent>();
        let _ = TypeId::of::<WebEventKind>();
    }

    #[test]
    fn crate_root_reexports_helpers_and_handlers() {
        let _queue = build_merge_queue;
        let _event_name = web_event_name;
        let _router: fn(WebState) -> axum::Router = router;
        let _spawn = spawn_sandbox_run;
        let _run_server = run_web_server;
    }
}
