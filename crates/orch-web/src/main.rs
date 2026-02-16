use std::path::PathBuf;

use orch_web::handler::{
    ApiState, handle_create_task, handle_delete_task, handle_get_session, handle_get_task,
    handle_health, handle_list_events, handle_list_sessions, handle_list_skills, handle_list_tasks,
    handle_resume_task, handle_stats, handle_stop_task, handle_task_events,
};
use orch_web::request::HttpMethod;
use orch_web::router::Router;
use orch_web::server::WebServer;

fn main() {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:3000".to_string());

    let mut router = Router::new();
    router.add_route(HttpMethod::GET, "/api/v1/tasks", handle_list_tasks);
    router.add_route(HttpMethod::GET, "/api/v1/tasks/:id", handle_get_task);
    router.add_route(HttpMethod::POST, "/api/v1/tasks", handle_create_task);
    router.add_route(HttpMethod::DELETE, "/api/v1/tasks/:id", handle_delete_task);
    router.add_route(HttpMethod::POST, "/api/v1/tasks/:id/stop", handle_stop_task);
    router.add_route(HttpMethod::POST, "/api/v1/tasks/:id/resume", handle_resume_task);
    router.add_route(HttpMethod::GET, "/api/v1/events", handle_list_events);
    router.add_route(HttpMethod::GET, "/api/v1/events/:task_id", handle_task_events);
    router.add_route(HttpMethod::GET, "/api/v1/stats", handle_stats);
    router.add_route(HttpMethod::GET, "/api/v1/sessions", handle_list_sessions);
    router.add_route(HttpMethod::GET, "/api/v1/sessions/:id", handle_get_session);
    router.add_route(HttpMethod::GET, "/api/v1/skills", handle_list_skills);
    router.add_route(HttpMethod::GET, "/api/v1/health", handle_health);

    let state = ApiState::new(
        PathBuf::from(".orch/state.sqlite"),
        PathBuf::from(".orch/events"),
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    );
    let server = WebServer::new(&addr).with_router(router).with_state(state);

    println!("Othala web API listening on {addr}");
    if let Err(err) = server.run() {
        eprintln!("web server error: {err}");
        std::process::exit(1);
    }
}
