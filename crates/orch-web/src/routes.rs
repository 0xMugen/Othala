use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::error::WebError;
use crate::merge_queue::build_merge_queue;
use crate::model::{
    web_event_name, SandboxDetailResponse, SandboxSpawnRequest, TaskDetailResponse,
    TaskListResponse, TaskView,
};
use crate::sandbox::spawn_sandbox_run;
use crate::state::WebState;

pub fn router(state: WebState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/api/tasks", get(list_tasks))
        .route("/api/tasks/{task_id}", get(get_task))
        .route("/api/merge-queue", get(merge_queue))
        .route("/api/sandbox", post(spawn_sandbox))
        .route("/api/sandbox/{sandbox_id}", get(get_sandbox))
        .route("/api/events", get(stream_events))
        .with_state(state)
}

async fn index() -> impl IntoResponse {
    "orch-web running"
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "ok": true }))
}

async fn list_tasks(State(state): State<WebState>) -> Result<Json<TaskListResponse>, WebError> {
    let tasks = state.list_tasks().await;
    let views = tasks.iter().map(TaskView::from).collect::<Vec<_>>();
    Ok(Json(TaskListResponse { tasks: views }))
}

async fn get_task(
    State(state): State<WebState>,
    Path(task_id): Path<String>,
) -> Result<Json<TaskDetailResponse>, WebError> {
    let task = state
        .task(&task_id)
        .await
        .ok_or_else(|| WebError::NotFound {
            resource: format!("task:{task_id}"),
        })?;
    Ok(Json(TaskDetailResponse {
        task: TaskView::from(&task),
    }))
}

async fn merge_queue(
    State(state): State<WebState>,
) -> Result<Json<crate::model::MergeQueueResponse>, WebError> {
    let tasks = state.list_tasks().await;
    Ok(Json(build_merge_queue(&tasks)))
}

async fn spawn_sandbox(
    State(state): State<WebState>,
    Json(request): Json<SandboxSpawnRequest>,
) -> Result<Json<crate::model::SandboxSpawnResponse>, WebError> {
    let response = spawn_sandbox_run(state, request).await?;
    Ok(Json(response))
}

async fn get_sandbox(
    State(state): State<WebState>,
    Path(sandbox_id): Path<String>,
) -> Result<Json<SandboxDetailResponse>, WebError> {
    let sandbox = state
        .sandbox(&sandbox_id)
        .await
        .ok_or_else(|| WebError::NotFound {
            resource: format!("sandbox:{sandbox_id}"),
        })?;
    Ok(Json(SandboxDetailResponse { sandbox }))
}

async fn stream_events(
    State(state): State<WebState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<SseEvent, Infallible>>> {
    let rx = state.subscribe();
    let stream = BroadcastStream::new(rx).map(|message| {
        let event = match message {
            Ok(payload) => {
                let data = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
                SseEvent::default()
                    .event(web_event_name(&payload.kind))
                    .data(data)
            }
            Err(_) => SseEvent::default().event("lagged").data("{}"),
        };
        Ok::<SseEvent, Infallible>(event)
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(10))
            .text("keepalive"),
    )
}

#[cfg(test)]
mod tests {
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use chrono::Utc;
    use orch_core::state::{ReviewCapacityState, ReviewStatus, TaskState, VerifyStatus};
    use orch_core::types::{RepoId, SubmitMode, Task, TaskId, TaskRole, TaskType};
    use std::path::PathBuf;
    use tower::ServiceExt;

    use crate::model::{
        MergeQueueResponse, SandboxDetailResponse, SandboxRunView, SandboxSpawnRequest,
        SandboxStatus, SandboxTarget, TaskDetailResponse, TaskListResponse,
    };
    use crate::state::WebState;

    use super::router;

    fn mk_task(id: &str, state: TaskState, depends_on: &[&str]) -> Task {
        Task {
            id: TaskId(id.to_string()),
            repo_id: RepoId("example".to_string()),
            title: format!("Task {id}"),
            state,
            role: TaskRole::General,
            task_type: TaskType::Feature,
            preferred_model: None,
            depends_on: depends_on
                .iter()
                .map(|parent| TaskId((*parent).to_string()))
                .collect(),
            submit_mode: SubmitMode::Single,
            branch_name: Some(format!("task/{id}")),
            worktree_path: PathBuf::from(format!(".orch/wt/{id}")),
            pr: None,
            verify_status: VerifyStatus::NotRun,
            review_status: ReviewStatus {
                required_models: Vec::new(),
                approvals_received: 0,
                approvals_required: 0,
                unanimous: false,
                capacity_state: ReviewCapacityState::Sufficient,
            },
            patch_ready: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn mk_sandbox(id: &str, status: SandboxStatus) -> SandboxRunView {
        SandboxRunView {
            sandbox_id: id.to_string(),
            target: SandboxTarget::Task {
                task_id: "T1".to_string(),
            },
            status,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            sandbox_path: Some(PathBuf::from(format!("/tmp/{id}"))),
            checkout_ref: Some("HEAD".to_string()),
            cleanup_worktree: true,
            worktree_cleaned: false,
            worktree_cleanup_error: None,
            logs: Vec::new(),
            last_error: None,
        }
    }

    #[tokio::test]
    async fn list_tasks_returns_task_views() {
        let state = WebState::default();
        state
            .upsert_task(mk_task("T1", TaskState::Running, &[]))
            .await;

        let app = router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/tasks")
                    .method("GET")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let payload: TaskListResponse = serde_json::from_slice(&body).expect("json payload");
        assert_eq!(payload.tasks.len(), 1);
        assert_eq!(payload.tasks[0].task_id, "T1");
        assert_eq!(payload.tasks[0].state, TaskState::Running);
    }

    #[tokio::test]
    async fn get_task_returns_not_found_error_for_unknown_task() {
        let state = WebState::default();
        let app = router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/tasks/T404")
                    .method("GET")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let payload: serde_json::Value = serde_json::from_slice(&body).expect("error json");
        assert_eq!(payload["code"], "not_found");
        let message = payload["message"].as_str().expect("message as str");
        assert!(message.contains("task:T404"));
    }

    #[tokio::test]
    async fn get_task_returns_task_detail_for_existing_task() {
        let state = WebState::default();
        state
            .upsert_task(mk_task("T42", TaskState::Reviewing, &["T1"]))
            .await;

        let app = router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/tasks/T42")
                    .method("GET")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let payload: TaskDetailResponse = serde_json::from_slice(&body).expect("json payload");
        assert_eq!(payload.task.task_id, "T42");
        assert_eq!(payload.task.state, TaskState::Reviewing);
        assert_eq!(payload.task.depends_on, vec!["T1".to_string()]);
    }

    #[tokio::test]
    async fn merge_queue_includes_only_awaiting_merge_tasks() {
        let state = WebState::default();
        state
            .upsert_task(mk_task("T1", TaskState::AwaitingMerge, &[]))
            .await;
        state
            .upsert_task(mk_task("T2", TaskState::Running, &["T1"]))
            .await;

        let app = router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/merge-queue")
                    .method("GET")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let payload: MergeQueueResponse = serde_json::from_slice(&body).expect("json payload");
        assert_eq!(payload.groups.len(), 1);
        assert_eq!(payload.groups[0].task_ids, vec!["T1".to_string()]);
        assert_eq!(
            payload.groups[0].recommended_merge_order,
            vec!["T1".to_string()]
        );
    }

    #[tokio::test]
    async fn get_sandbox_returns_not_found_for_unknown_id() {
        let state = WebState::default();
        let app = router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/sandbox/SBX-404")
                    .method("GET")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let payload: serde_json::Value = serde_json::from_slice(&body).expect("error json");
        assert_eq!(payload["code"], "not_found");
        let message = payload["message"].as_str().expect("message as str");
        assert!(message.contains("sandbox:SBX-404"));
    }

    #[tokio::test]
    async fn get_sandbox_returns_detail_for_existing_id() {
        let state = WebState::default();
        state
            .upsert_sandbox(mk_sandbox("SBX-1", SandboxStatus::Running))
            .await;

        let app = router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/sandbox/SBX-1")
                    .method("GET")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let payload: SandboxDetailResponse =
            serde_json::from_slice(&body).expect("sandbox detail json");
        assert_eq!(payload.sandbox.sandbox_id, "SBX-1");
        assert_eq!(payload.sandbox.status, SandboxStatus::Running);
    }

    #[tokio::test]
    async fn spawn_sandbox_rejects_empty_verify_commands() {
        let state = WebState::default();
        let app = router(state);

        let request = SandboxSpawnRequest {
            target: SandboxTarget::Task {
                task_id: "T1".to_string(),
            },
            repo_path: PathBuf::from("/tmp/repo"),
            nix_dev_shell: "nix develop".to_string(),
            verify_full_commands: Vec::new(),
            checkout_ref: None,
            cleanup_worktree: true,
        };
        let body = serde_json::to_vec(&request).expect("request json");
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/sandbox")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let payload: serde_json::Value = serde_json::from_slice(&body).expect("error json");
        assert_eq!(payload["code"], "bad_request");
        let message = payload["message"].as_str().expect("message as str");
        assert!(message.contains("verify_full_commands"));
    }

    #[tokio::test]
    async fn spawn_sandbox_rejects_empty_nix_dev_shell() {
        let state = WebState::default();
        let app = router(state);

        let request = SandboxSpawnRequest {
            target: SandboxTarget::Task {
                task_id: "T1".to_string(),
            },
            repo_path: PathBuf::from("/tmp/repo"),
            nix_dev_shell: "   ".to_string(),
            verify_full_commands: vec!["echo ok".to_string()],
            checkout_ref: None,
            cleanup_worktree: true,
        };
        let body = serde_json::to_vec(&request).expect("request json");
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/sandbox")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let payload: serde_json::Value = serde_json::from_slice(&body).expect("error json");
        assert_eq!(payload["code"], "bad_request");
        let message = payload["message"].as_str().expect("message as str");
        assert!(message.contains("nix_dev_shell"));
    }

    #[tokio::test]
    async fn health_endpoint_returns_ok_true() {
        let state = WebState::default();
        let app = router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .method("GET")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let payload: serde_json::Value = serde_json::from_slice(&body).expect("health json");
        assert_eq!(payload["ok"], true);
    }

    #[tokio::test]
    async fn index_endpoint_returns_running_text() {
        let state = WebState::default();
        let app = router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .method("GET")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let text = String::from_utf8(body.to_vec()).expect("utf8 body");
        assert_eq!(text, "orch-web running");
    }

    #[tokio::test]
    async fn unknown_route_returns_404() {
        let state = WebState::default();
        let app = router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/missing")
                    .method("GET")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
