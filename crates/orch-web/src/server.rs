use axum::serve;
use tokio::net::TcpListener;

use crate::error::WebError;
use crate::routes::router;
use crate::state::WebState;

pub async fn run_web_server(bind_addr: &str, state: WebState) -> Result<(), WebError> {
    let listener = TcpListener::bind(bind_addr).await?;
    serve(listener, router(state))
        .await
        .map_err(|err| WebError::Internal {
            message: err.to_string(),
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tokio::net::TcpListener;

    use crate::error::WebError;
    use crate::state::WebState;

    use super::run_web_server;

    #[tokio::test]
    async fn run_web_server_returns_io_error_for_invalid_bind_address() {
        let err = run_web_server("not-a-valid-bind-addr", WebState::default())
            .await
            .expect_err("invalid bind address should fail");
        assert!(matches!(err, WebError::Io { .. }));
    }

    #[tokio::test]
    async fn run_web_server_returns_io_error_when_port_is_already_in_use() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind probe listener");
        let addr = listener.local_addr().expect("probe local addr");
        let bind = addr.to_string();

        let err = run_web_server(&bind, WebState::default())
            .await
            .expect_err("address in use should fail");
        assert!(matches!(err, WebError::Io { .. }));
    }
}
