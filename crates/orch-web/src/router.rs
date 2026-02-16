use std::collections::HashMap;

use crate::handler::ApiState;
use crate::request::{HttpMethod, HttpRequest};
use crate::response::HttpResponse;

pub type PathParams = HashMap<String, String>;
pub type HandlerFn = fn(&HttpRequest, &ApiState, &PathParams) -> HttpResponse;

#[derive(Debug, Clone)]
pub struct Route {
    pub path_pattern: String,
    pub method: HttpMethod,
    pub handler: HandlerFn,
}

#[derive(Debug, Clone, Default)]
pub struct Router {
    routes: Vec<Route>,
}

pub struct RouteMatch {
    pub handler: HandlerFn,
    pub params: PathParams,
}

impl Router {
    pub fn new() -> Self {
        Self { routes: Vec::new() }
    }

    pub fn add_route(&mut self, method: HttpMethod, path_pattern: &str, handler: HandlerFn) {
        self.routes.push(Route {
            path_pattern: path_pattern.to_string(),
            method,
            handler,
        });
    }

    pub fn match_route(&self, method: &HttpMethod, path: &str) -> Option<RouteMatch> {
        for route in &self.routes {
            if route.method != *method {
                continue;
            }

            if let Some(params) = match_path(&route.path_pattern, path) {
                return Some(RouteMatch {
                    handler: route.handler,
                    params,
                });
            }
        }

        None
    }
}

fn match_path(pattern: &str, path: &str) -> Option<PathParams> {
    let pattern_segments = split_segments(pattern);
    let path_segments = split_segments(path);

    if pattern_segments.len() != path_segments.len() {
        return None;
    }

    let mut params = HashMap::new();
    for (pattern_segment, path_segment) in pattern_segments.iter().zip(path_segments.iter()) {
        if let Some(name) = pattern_segment.strip_prefix(':') {
            params.insert(name.to_string(), (*path_segment).to_string());
            continue;
        }

        if pattern_segment != path_segment {
            return None;
        }
    }

    Some(params)
}

fn split_segments(path: &str) -> Vec<&str> {
    path.trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::handler::{ApiState, handle_health};
    use crate::request::{HttpMethod, HttpRequest};

    use super::Router;

    fn dummy_request() -> HttpRequest {
        HttpRequest {
            method: HttpMethod::GET,
            path: "/api/v1/health".to_string(),
            query_params: std::collections::HashMap::new(),
            headers: std::collections::HashMap::new(),
            body: None,
        }
    }

    #[test]
    fn matches_exact_route() {
        let mut router = Router::new();
        router.add_route(HttpMethod::GET, "/api/v1/health", handle_health);

        let matched = router.match_route(&HttpMethod::GET, "/api/v1/health");

        assert!(matched.is_some());
    }

    #[test]
    fn extracts_path_params() {
        let mut router = Router::new();
        router.add_route(HttpMethod::GET, "/api/v1/tasks/:id", handle_health);

        let matched = router
            .match_route(&HttpMethod::GET, "/api/v1/tasks/task-123")
            .expect("route should match");

        assert_eq!(matched.params.get("id").map(String::as_str), Some("task-123"));
    }

    #[test]
    fn does_not_match_when_method_differs() {
        let mut router = Router::new();
        router.add_route(HttpMethod::GET, "/api/v1/tasks/:id", handle_health);

        let matched = router.match_route(&HttpMethod::POST, "/api/v1/tasks/task-123");

        assert!(matched.is_none());
    }

    #[test]
    fn does_not_match_unknown_path() {
        let mut router = Router::new();
        router.add_route(HttpMethod::GET, "/api/v1/health", handle_health);

        let matched = router.match_route(&HttpMethod::GET, "/api/v1/unknown");

        assert!(matched.is_none());
    }

    #[test]
    fn matched_handler_is_callable() {
        let mut router = Router::new();
        router.add_route(HttpMethod::GET, "/api/v1/health", handle_health);

        let matched = router
            .match_route(&HttpMethod::GET, "/api/v1/health")
            .expect("route should match");
        let response = (matched.handler)(
            &dummy_request(),
            &ApiState::default(),
            &std::collections::HashMap::new(),
        );

        assert_eq!(response.status_code, 200);
    }
}
