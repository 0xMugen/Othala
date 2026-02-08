use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum WebError {
    #[error("resource not found: {resource}")]
    NotFound { resource: String },
    #[error("invalid request: {message}")]
    BadRequest { message: String },
    #[error("io error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },
    #[error("internal error: {message}")]
    Internal { message: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            WebError::NotFound { resource } => (
                StatusCode::NOT_FOUND,
                "not_found",
                format!("resource not found: {resource}"),
            ),
            WebError::BadRequest { message } => (StatusCode::BAD_REQUEST, "bad_request", message),
            WebError::Io { source } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "io_error",
                source.to_string(),
            ),
            WebError::Internal { message } => {
                (StatusCode::INTERNAL_SERVER_ERROR, "internal_error", message)
            }
        };

        let body = ErrorBody {
            code: code.to_string(),
            message,
        };
        (status, Json(body)).into_response()
    }
}
