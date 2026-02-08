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
    web_event_name, SandboxDetailResponse, SandboxSpawnRequest, TaskDetailResponse, TaskListResponse,
    TaskView,
};
use crate::sandbox::spawn_sandbox_run;
use crate::state::WebState;

pub fn router(state: WebState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/api/tasks", get(list_tasks))
        .route("/api/tasks/:task_id", get(get_task))
        .route("/api/merge-queue", get(merge_queue))
        .route("/api/sandbox", post(spawn_sandbox))
        .route("/api/sandbox/:sandbox_id", get(get_sandbox))
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

async fn merge_queue(State(state): State<WebState>) -> Result<Json<crate::model::MergeQueueResponse>, WebError> {
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
