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
