mod config;

use axum::Router;
use config::AppConfig;
use geo_api::{AppState, router};
use std::error::Error;
use tokio::net::TcpListener;
use tracing::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt::init();
    let config = AppConfig::from_env()?;
    if !config.dev_scope_headers {
        return Err("GEO_DEV_SCOPE_HEADERS=false is unsupported until authenticated server-side scope resolution is installed".into());
    }

    let state = AppState::development();
    if config.ready_on_start {
        state.set_ready(true);
    }
    let app: Router = router(state);
    let listener = TcpListener::bind(config.bind_addr).await?;
    info!(
        address = %config.bind_addr,
        dev_scope_headers = config.dev_scope_headers,
        durable_storage = false,
        "starting GEO API with in-memory development stores"
    );
    axum::serve(listener, app).await?;
    Ok(())
}
