use std::collections::HashMap;
use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::request::HttpRequest;
use crate::response::{HttpResponse, error_response, json_response};
use crate::router::PathParams;

#[derive(Debug, Clone)]
pub struct ApiState {
    pub sqlite_path: PathBuf,
    pub event_log_root: PathBuf,
    pub repo_root: PathBuf,
}

impl ApiState {
    pub fn new(sqlite_path: PathBuf, event_log_root: PathBuf, repo_root: PathBuf) -> Self {
        Self {
            sqlite_path,
            event_log_root,
            repo_root,
        }
    }
}

impl Default for ApiState {
    fn default() -> Self {
        Self {
            sqlite_path: PathBuf::from(".orch/state.sqlite"),
            event_log_root: PathBuf::from(".orch/events"),
            repo_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct ApiTask {
    id: String,
    repo_id: String,
    title: String,
    state: String,
    preferred_model: Option<String>,
    priority: String,
    created_at: chrono::DateTime<Utc>,
    updated_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct CreateTaskRequest {
    repo: String,
    title: String,
    model: Option<String>,
    priority: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ApiEvent {
    id: String,
    task_id: String,
    kind: String,
    message: String,
    timestamp: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
struct ApiSession {
    id: String,
    title: String,
    created_at: chrono::DateTime<Utc>,
    updated_at: chrono::DateTime<Utc>,
    task_ids: Vec<String>,
    parent_session_id: Option<String>,
    status: String,
}

#[derive(Debug, Clone, Serialize)]
struct ApiSkill {
    name: String,
    description: String,
    content: String,
    source_path: String,
    tags: Vec<String>,
}

pub fn handle_list_tasks(_request: &HttpRequest, _state: &ApiState, _params: &PathParams) -> HttpResponse {
    json_response(200, &sample_tasks())
}

pub fn handle_get_task(_request: &HttpRequest, _state: &ApiState, params: &PathParams) -> HttpResponse {
    let Some(task_id) = params.get("id") else {
        return error_response(400, "missing task id");
    };

    match sample_tasks().into_iter().find(|task| task.id == *task_id) {
        Some(task) => json_response(200, &task),
        None => error_response(404, &format!("task '{task_id}' not found")),
    }
}

pub fn handle_create_task(request: &HttpRequest, _state: &ApiState, _params: &PathParams) -> HttpResponse {
    let Some(raw) = request.body.as_deref() else {
        return error_response(400, "missing request body");
    };

    let payload: CreateTaskRequest = match serde_json::from_str(raw) {
        Ok(value) => value,
        Err(err) => return error_response(400, &format!("invalid json body: {err}")),
    };

    if payload.repo.trim().is_empty() || payload.title.trim().is_empty() {
        return error_response(400, "repo and title are required");
    }

    let now = Utc::now();
    let task = ApiTask {
        id: format!("task-{}", now.timestamp_nanos_opt().unwrap_or_default()),
        repo_id: payload.repo,
        title: payload.title,
        state: "chatting".to_string(),
        preferred_model: payload.model,
        priority: payload.priority.unwrap_or_else(|| "normal".to_string()),
        created_at: now,
        updated_at: now,
    };

    json_response(201, &task)
}

pub fn handle_delete_task(_request: &HttpRequest, _state: &ApiState, params: &PathParams) -> HttpResponse {
    let Some(task_id) = params.get("id") else {
        return error_response(400, "missing task id");
    };

    json_response(200, &serde_json::json!({ "deleted": true, "task_id": task_id }))
}

pub fn handle_stop_task(_request: &HttpRequest, _state: &ApiState, params: &PathParams) -> HttpResponse {
    task_action_response(params, "stopped")
}

pub fn handle_resume_task(_request: &HttpRequest, _state: &ApiState, params: &PathParams) -> HttpResponse {
    task_action_response(params, "ready")
}

pub fn handle_list_events(_request: &HttpRequest, _state: &ApiState, _params: &PathParams) -> HttpResponse {
    json_response(200, &sample_events())
}

pub fn handle_task_events(_request: &HttpRequest, _state: &ApiState, params: &PathParams) -> HttpResponse {
    let Some(task_id) = params.get("task_id") else {
        return error_response(400, "missing task id");
    };

    let events: Vec<ApiEvent> = sample_events()
        .into_iter()
        .filter(|event| event.task_id == *task_id)
        .collect();
    json_response(200, &events)
}

pub fn handle_stats(_request: &HttpRequest, _state: &ApiState, _params: &PathParams) -> HttpResponse {
    let tasks = sample_tasks();
    let events = sample_events();
    let sessions = sample_sessions();

    let stopped_count = tasks.iter().filter(|task| task.state == "stopped").count();
    let ready_count = tasks.iter().filter(|task| task.state == "ready").count();

    json_response(
        200,
        &serde_json::json!({
            "task_count": tasks.len(),
            "ready_task_count": ready_count,
            "stopped_task_count": stopped_count,
            "event_count": events.len(),
            "session_count": sessions.len()
        }),
    )
}

pub fn handle_list_sessions(_request: &HttpRequest, _state: &ApiState, _params: &PathParams) -> HttpResponse {
    json_response(200, &sample_sessions())
}

pub fn handle_get_session(_request: &HttpRequest, _state: &ApiState, params: &PathParams) -> HttpResponse {
    let Some(session_id) = params.get("id") else {
        return error_response(400, "missing session id");
    };

    match sample_sessions().into_iter().find(|session| session.id == *session_id) {
        Some(session) => json_response(200, &session),
        None => error_response(404, &format!("session '{session_id}' not found")),
    }
}

pub fn handle_list_skills(_request: &HttpRequest, state: &ApiState, _params: &PathParams) -> HttpResponse {
    let skills = vec![ApiSkill {
        name: "rust-refactor".to_string(),
        description: "Refactor Rust code while preserving behavior".to_string(),
        content: "Skill content is loaded from markdown definitions".to_string(),
        source_path: state
            .repo_root
            .join(".othala/skills/rust-refactor/SKILL.md")
            .display()
            .to_string(),
        tags: vec!["rust".to_string(), "refactor".to_string()],
    }];

    json_response(200, &skills)
}

pub fn handle_health(_request: &HttpRequest, state: &ApiState, _params: &PathParams) -> HttpResponse {
    json_response(
        200,
        &serde_json::json!({
            "status": "ok",
            "timestamp": Utc::now(),
            "sqlite_path": state.sqlite_path,
            "event_log_root": state.event_log_root,
        }),
    )
}

fn task_action_response(params: &HashMap<String, String>, state: &str) -> HttpResponse {
    let Some(task_id) = params.get("id") else {
        return error_response(400, "missing task id");
    };

    json_response(200, &serde_json::json!({ "task_id": task_id, "state": state }))
}

fn sample_tasks() -> Vec<ApiTask> {
    let now = Utc::now();
    vec![
        ApiTask {
            id: "task-1".to_string(),
            repo_id: "othala".to_string(),
            title: "Implement HTTP server".to_string(),
            state: "ready".to_string(),
            preferred_model: Some("codex".to_string()),
            priority: "high".to_string(),
            created_at: now,
            updated_at: now,
        },
        ApiTask {
            id: "task-2".to_string(),
            repo_id: "othala".to_string(),
            title: "Review merge queue".to_string(),
            state: "stopped".to_string(),
            preferred_model: Some("claude".to_string()),
            priority: "normal".to_string(),
            created_at: now,
            updated_at: now,
        },
    ]
}

fn sample_events() -> Vec<ApiEvent> {
    let now = Utc::now();
    vec![
        ApiEvent {
            id: "evt-1".to_string(),
            task_id: "task-1".to_string(),
            kind: "task.created".to_string(),
            message: "Task created".to_string(),
            timestamp: now,
        },
        ApiEvent {
            id: "evt-2".to_string(),
            task_id: "task-1".to_string(),
            kind: "task.state_changed".to_string(),
            message: "Task moved to ready".to_string(),
            timestamp: now,
        },
        ApiEvent {
            id: "evt-3".to_string(),
            task_id: "task-2".to_string(),
            kind: "task.stopped".to_string(),
            message: "Task stopped".to_string(),
            timestamp: now,
        },
    ]
}

fn sample_sessions() -> Vec<ApiSession> {
    let now = Utc::now();
    vec![ApiSession {
        id: "session-1".to_string(),
        title: "Sprint planning".to_string(),
        created_at: now,
        updated_at: now,
        task_ids: vec!["task-1".to_string(), "task-2".to_string()],
        parent_session_id: None,
        status: "active".to_string(),
    }]
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::request::{HttpMethod, HttpRequest};

    use super::{ApiState, handle_create_task, handle_get_task, handle_health, handle_list_tasks};

    fn request(method: HttpMethod, body: Option<&str>) -> HttpRequest {
        HttpRequest {
            method,
            path: "/".to_string(),
            query_params: HashMap::new(),
            headers: HashMap::new(),
            body: body.map(ToString::to_string),
        }
    }

    #[test]
    fn list_tasks_returns_json_array() {
        let response = handle_list_tasks(&request(HttpMethod::GET, None), &ApiState::default(), &HashMap::new());

        assert_eq!(response.status_code, 200);
        let value: serde_json::Value = serde_json::from_str(&response.body).expect("valid json");
        assert!(value.is_array());
    }

    #[test]
    fn get_task_returns_not_found_for_unknown_id() {
        let mut params = HashMap::new();
        params.insert("id".to_string(), "does-not-exist".to_string());

        let response = handle_get_task(&request(HttpMethod::GET, None), &ApiState::default(), &params);

        assert_eq!(response.status_code, 404);
    }

    #[test]
    fn create_task_requires_body() {
        let response = handle_create_task(&request(HttpMethod::POST, None), &ApiState::default(), &HashMap::new());

        assert_eq!(response.status_code, 400);
    }

    #[test]
    fn create_task_returns_created_task() {
        let body = r#"{"repo":"othala","title":"New task","model":"codex","priority":"high"}"#;
        let response = handle_create_task(
            &request(HttpMethod::POST, Some(body)),
            &ApiState::default(),
            &HashMap::new(),
        );

        assert_eq!(response.status_code, 201);
        let value: serde_json::Value = serde_json::from_str(&response.body).expect("valid json");
        assert_eq!(value.get("repo_id").and_then(serde_json::Value::as_str), Some("othala"));
        assert_eq!(value.get("title").and_then(serde_json::Value::as_str), Some("New task"));
    }

    #[test]
    fn health_includes_status_and_paths() {
        let response = handle_health(&request(HttpMethod::GET, None), &ApiState::default(), &HashMap::new());

        assert_eq!(response.status_code, 200);
        let value: serde_json::Value = serde_json::from_str(&response.body).expect("valid json");
        assert_eq!(value.get("status").and_then(serde_json::Value::as_str), Some("ok"));
        assert!(value.get("sqlite_path").is_some());
        assert!(value.get("event_log_root").is_some());
    }
}
