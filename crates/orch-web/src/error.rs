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

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use axum::response::IntoResponse;
    use serde_json::Value;

    use super::WebError;

    #[tokio::test]
    async fn not_found_maps_to_404_and_not_found_code() {
        let response = WebError::NotFound {
            resource: "task:T404".to_string(),
        }
        .into_response();

        assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let payload: Value = serde_json::from_slice(&body).expect("json payload");
        assert_eq!(payload["code"], "not_found");
        let message = payload["message"].as_str().expect("message as string");
        assert!(message.contains("task:T404"));
    }

    #[tokio::test]
    async fn bad_request_maps_to_400_and_bad_request_code() {
        let response = WebError::BadRequest {
            message: "invalid payload".to_string(),
        }
        .into_response();

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let payload: Value = serde_json::from_slice(&body).expect("json payload");
        assert_eq!(payload["code"], "bad_request");
        assert_eq!(payload["message"], "invalid payload");
    }

    #[tokio::test]
    async fn io_error_maps_to_500_and_io_error_code() {
        let response = WebError::Io {
            source: std::io::Error::other("disk offline"),
        }
        .into_response();

        assert_eq!(
            response.status(),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let payload: Value = serde_json::from_slice(&body).expect("json payload");
        assert_eq!(payload["code"], "io_error");
        let message = payload["message"].as_str().expect("message as string");
        assert!(message.contains("disk offline"));
    }

    #[tokio::test]
    async fn internal_error_maps_to_500_and_internal_error_code() {
        let response = WebError::Internal {
            message: "unexpected".to_string(),
        }
        .into_response();

        assert_eq!(
            response.status(),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let payload: Value = serde_json::from_slice(&body).expect("json payload");
        assert_eq!(payload["code"], "internal_error");
        assert_eq!(payload["message"], "unexpected");
    }

    #[tokio::test]
    async fn not_found_message_uses_resource_not_found_prefix() {
        let response = WebError::NotFound {
            resource: "sandbox:SBX-9".to_string(),
        }
        .into_response();
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let payload: Value = serde_json::from_slice(&body).expect("json payload");
        assert_eq!(
            payload["message"],
            "resource not found: sandbox:SBX-9".to_string()
        );
    }

    #[tokio::test]
    async fn io_error_from_conversion_maps_to_io_error_code() {
        let io = std::io::Error::other("permission denied");
        let web: WebError = io.into();
        let response = web.into_response();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let payload: Value = serde_json::from_slice(&body).expect("json payload");
        assert_eq!(payload["code"], "io_error");
        let message = payload["message"].as_str().expect("message as str");
        assert!(message.contains("permission denied"));
    }
}
