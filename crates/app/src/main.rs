mod config;

use axum::Router;
use config::AppConfig;
use geo_api::{AppState, router};
use geo_persistence::Database;
use std::error::Error;
use tokio::net::TcpListener;
use tracing::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt::init();
    let config = AppConfig::from_env()?;
    let (state, durable_storage) = if AppConfig::database_url_configured() {
        // A configured database is authoritative.  Connection or migration
        // failures terminate startup; the process never falls back to memory
        // authentication or idempotency state.
        let database = Database::connect_and_migrate_from_env().await?;
        let state =
            AppState::from_database(&database).with_allowed_origins(config.allowed_origins.clone());
        state.set_ready(true);
        (state, true)
    } else {
        let password = config.validate_for_memory_mode()?;
        let state = AppState::development_with_password(password)
            .with_allowed_origins(config.allowed_origins.clone());
        if config.ready_on_start {
            state.set_ready(true);
        }
        (state, false)
    };
    let app: Router = router(state);
    let listener = TcpListener::bind(config.bind_addr).await?;
    info!(
        address = %config.bind_addr,
        durable_storage,
        "starting GEO API"
    );
    axum::serve(listener, app).await?;
    Ok(())
}
