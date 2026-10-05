mod agent_dispatch;
mod bootstrap;
mod channels;
mod config;
mod content_dispatch;
#[cfg(test)]
mod content_runtime_tests;
mod dispatch;
mod production_runtime;
mod publication_lookup_dispatch;
mod runtime;
mod verification_dispatch;

use axum::Router;
use config::AppConfig;
use geo_api::{AppState, EmbeddedAgentRuntime, reduce_cycle_report, router};
use geo_persistence::{Database, PgProjectRepository, PgReportRepository};
use std::error::Error;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt::init();
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args == [std::ffi::OsString::from("--bootstrap")] {
        bootstrap::run_from_env().await?;
        return Ok(());
    }
    if !args.is_empty() {
        return Err("unsupported command; use --bootstrap or no arguments".into());
    }
    let config = AppConfig::from_env()?;
    let durable_storage = AppConfig::database_url_configured();
    config.validate_ai_mode(durable_storage)?;
    let production_ai = production_runtime::ProductionAiConfig::from_env()?;
    if production_ai.is_some() && !durable_storage {
        return Err("production model configuration requires PostgreSQL".into());
    }
    let (state, durable_storage, content_scanner, agent_scanner) = if durable_storage {
        // A configured database is authoritative.  Connection or migration
        // failures terminate startup; the process never falls back to memory
        // authentication or idempotency state.
        let database = Database::connect_and_migrate_from_env().await?;
        let report_scanner = PgReportRepository::from_database(&database);
        let agent_scanner = geo_persistence::PgAgentRepository::from_database(&database);
        let content_scanner = geo_persistence::PgContentRepository::from_database(&database);
        let cycle_scanner = PgProjectRepository::from_database(&database);
        let verification_scanner =
            geo_persistence::PgConnectorCapabilityRepository::from_database(&database);
        let state = channels::configure(
            AppState::from_database(&database).with_allowed_origins(config.allowed_origins.clone()),
        )?;
        let runtime = if let Some(ai) = production_ai.as_ref() {
            let provider = production_runtime::build_model_provider(&database, ai)?;
            runtime::assemble_with_provider(&state, &ai.bundle_path, &ai.bundle_sha256, provider)?
        } else {
            Arc::new(EmbeddedAgentRuntime::unconfigured())
        };
        let state = state.with_agent_runtime(runtime);
        if config.single_process_executor {
            let reconciled = state.reconcile_running_runs().await?;
            info!(
                reconciled,
                "reconciled abandoned agent runs during single-process startup"
            );
        }
        state.set_ready(true);
        publication_lookup_dispatch::spawn(
            state.clone(),
            geo_persistence::PgPublicationLookupRepository::from_database(&database),
        );
        spawn_due_report_scanner(state.clone(), report_scanner, cycle_scanner.clone());
        verification_dispatch::spawn(verification_scanner);
        (
            state,
            true,
            Some((content_scanner, cycle_scanner)),
            Some(agent_scanner),
        )
    } else {
        let password = config.validate_for_memory_mode()?;
        let state = channels::configure(
            AppState::development_with_password(password)
                .with_allowed_origins(config.allowed_origins.clone()),
        )?;
        let runtime = runtime::assemble(&state, config.development_ai.as_ref())?;
        let state = state.with_agent_runtime(runtime);
        if config.single_process_executor {
            let reconciled = state.reconcile_running_runs().await?;
            info!(
                reconciled,
                "reconciled abandoned agent runs during single-process startup"
            );
        }
        if config.ready_on_start {
            state.set_ready(true);
        }
        (state, false, None, None)
    };
    runtime::configure_content_workflow(&state)?;
    if let Some(scanner) = agent_scanner {
        agent_dispatch::spawn(state.clone(), scanner);
    }
    if let Some((scanner, cycles)) = content_scanner {
        content_dispatch::spawn(state.clone(), scanner, cycles);
    }
    dispatch::spawn(state.clone());
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

/// PostgreSQL is authoritative for due-cycle enumeration. Each candidate is
/// reduced through the same scoped service as HTTP and agent tools; concurrent
/// replicas race safely at the repository's immutable report key.
fn spawn_due_report_scanner(
    state: AppState,
    scanner: PgReportRepository,
    cycle_scanner: PgProjectRepository,
) {
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            ticks.tick().await;
            let now = chrono::Utc::now();
            let mut cursor = None;
            // A failed old cycle must not monopolize the bounded first page.
            // Keep moving through this due set, then begin at the oldest
            // again on the next tick to retry transient failures.
            loop {
                match scanner.due_scopes_after(now, cursor).await {
                    Ok(candidates) => {
                        let count = candidates.len();
                        for (cutoff, scope, cycle_id) in candidates {
                            cursor = Some((cutoff, cycle_id));
                            if let Err(error) =
                                reduce_cycle_report(&state, &scope, cycle_id, None, now).await
                            {
                                // No account identity, source content, or other
                                // private operational context enters logs.
                                warn!(code = ?error.code, "due report reduction failed");
                            }
                        }
                        if count < 100 {
                            break;
                        }
                    }
                    Err(error) => {
                        warn!(code = ?error.code, "due report scan failed");
                        break;
                    }
                }
            }
            // A crash after saving a report must not lose its successor.
            // Keyset progress skips failed candidates rather than letting
            // an old page permanently starve later customer projects.
            let mut after = None;
            loop {
                match cycle_scanner
                    .list_pending_successor_cycles_after(100, after)
                    .await
                {
                    Ok(candidates) => {
                        let count = candidates.len();
                        for candidate in candidates {
                            after = Some(candidate.predecessor_cycle_id);
                            let Some(project_id) = candidate.scope.project_id else {
                                continue;
                            };
                            if let Err(error) = state
                                .project_repository()
                                .schedule_next_cycle(
                                    &candidate.scope,
                                    project_id,
                                    candidate.predecessor_cycle_id,
                                    now,
                                )
                                .await
                            {
                                warn!(code = ?error.code, "report successor recovery failed");
                            }
                        }
                        if count < 100 {
                            break;
                        }
                    }
                    Err(error) => {
                        warn!(code = ?error.code, "report successor scan failed");
                        break;
                    }
                }
            }
        }
    });
}
