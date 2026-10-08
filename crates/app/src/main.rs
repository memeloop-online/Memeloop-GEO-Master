mod agent_dispatch;
mod bootstrap;
mod channels;
mod config;
mod content_dispatch;
mod content_request_dispatch;
#[cfg(test)]
mod content_runtime_tests;
mod dispatch;
mod production_runtime;
mod provider_conversation_cleanup_dispatch;
mod publication_lookup_dispatch;
mod runtime;
mod scoped_test_runtime;
mod verification_dispatch;

use axum::Router;
use config::AppConfig;
use geo_api::{
    AppState, EmbeddedAgentRuntime, OFFICE_PARSER_PROFILE, OfficeParserClient, PDF_PARSER_PROFILE,
    PdfParserClient, reduce_cycle_report, router, spawn_office_parse_scanner,
    spawn_pdf_parse_scanner,
};
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
    let pdf_parser = match std::env::var_os("GEO_PDF_PARSER_URL") {
        None => None,
        Some(endpoint) => {
            let endpoint = endpoint
                .into_string()
                .map_err(|_| "invalid PDF parser configuration")?;
            let parser = PdfParserClient::new(&endpoint)?;
            parser.check_ready().await?;
            Some(parser)
        }
    };
    let office_parser = match std::env::var_os("GEO_OFFICE_PARSER_URL") {
        None => None,
        Some(endpoint) => {
            let endpoint = endpoint
                .into_string()
                .map_err(|_| "invalid Office parser configuration")?;
            let parser = OfficeParserClient::new(&endpoint)?;
            parser.check_ready().await?;
            Some(parser)
        }
    };
    let durable_storage = AppConfig::database_url_configured();
    let production_ai = production_runtime::ProductionAiConfig::from_env()?;
    config.validate_ai_mode(durable_storage, production_ai.is_some())?;
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
        let state = AppState::from_database_with_parser_profiles(
            &database,
            pdf_parser.as_ref().map(|_| PDF_PARSER_PROFILE.to_owned()),
            office_parser
                .as_ref()
                .map(|_| OFFICE_PARSER_PROFILE.to_owned()),
        );
        let state =
            channels::configure(state.with_allowed_origins(config.allowed_origins.clone()))?;
        let state = channels::configure_callbacks(state, &database)?;
        let runtime = if let Some(ai) = production_ai.as_ref() {
            let provider = production_runtime::build_model_provider(&database, ai)?;
            runtime::assemble_with_provider(&state, &ai.bundle_path, &ai.bundle_sha256, provider)?
        } else if let Some(ai) = config.scoped_test_ai.as_ref() {
            scoped_test_runtime::assemble(&state, ai)?
        } else if let (Some(ai), Some(scope)) = (
            config.development_ai.as_ref(),
            config.persistent_dev_ai_scope.as_ref(),
        ) {
            runtime::assemble_persistent(&state, ai, scope)?
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
        provider_conversation_cleanup_dispatch::spawn(
            state.clone(),
            geo_persistence::PgProviderConversationCleanupRepository::from_database(&database),
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
        let state = AppState::development_with_parser_profiles(
            password,
            pdf_parser.as_ref().map(|_| PDF_PARSER_PROFILE.to_owned()),
            office_parser
                .as_ref()
                .map(|_| OFFICE_PARSER_PROFILE.to_owned()),
        );
        let state = if let Ok(login_name) = std::env::var("GEO_DEV_LOGIN_NAME") {
            let display_name = std::env::var("GEO_DEV_DISPLAY_NAME")
                .unwrap_or_else(|_| "Local workspace".to_owned());
            let repository = geo_domain::MemoryAuthRepository::new();
            let operator = geo_domain::Operator::new(
                geo_domain::DEVELOPMENT_OPERATOR_ID,
                "local-workspace",
                &display_name,
            )?;
            let user = geo_domain::User::new(
                uuid::Uuid::from_u128(0x00000000000040008000000000000004).into(),
                operator.id,
                login_name,
                &display_name,
                password,
            )?;
            let mut membership = geo_domain::Membership::new(
                user.id,
                operator.id,
                geo_domain::DEVELOPMENT_TENANT_ID,
                geo_domain::Role::CustomerAdmin,
            );
            membership.tenant_slug = "local-workspace".to_owned();
            membership.tenant_display_name = display_name;
            let hosts = local_development_hosts(&config.allowed_origins);
            repository.insert_operator(operator, &hosts).await?;
            repository.insert_user(user).await?;
            repository.insert_membership(membership).await?;
            state.with_auth_repository(std::sync::Arc::new(repository))
        } else {
            state
        };
        let state =
            channels::configure(state.with_allowed_origins(config.allowed_origins.clone()))?;
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
    if let Some(parser) = pdf_parser {
        spawn_pdf_parse_scanner(state.clone(), parser);
    }
    if let Some(parser) = office_parser {
        spawn_office_parse_scanner(state.clone(), parser);
    }
    if let Some(scanner) = agent_scanner {
        agent_dispatch::spawn(state.clone(), scanner);
    }
    if let Some((scanner, cycles)) = content_scanner {
        content_dispatch::spawn(state.clone(), scanner, cycles);
    }
    dispatch::spawn(state.clone());
    content_request_dispatch::spawn(state.clone());
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

// The explicitly configured in-memory development identity may be served on
// isolated local test ports. Do not bind it to non-loopback tenant domains.
fn local_development_hosts(origins: &[String]) -> Vec<String> {
    let mut hosts = [
        "localhost",
        "localhost:5173",
        "localhost:8080",
        "127.0.0.1",
        "127.0.0.1:5173",
        "127.0.0.1:8080",
    ]
    .map(str::to_owned)
    .to_vec();
    hosts.extend(local_origin_hosts(origins));
    // The memory auth repository intentionally rejects duplicate host
    // assignments. AppConfig's default origins overlap these seeded hosts;
    // deduplicate *within this identity* using the repository's exact
    // normalization, without weakening conflicts across operators.
    let mut seen = std::collections::HashSet::new();
    hosts.retain(|host| seen.insert(geo_domain::normalize_host(host)));
    hosts
}

fn local_origin_hosts(origins: &[String]) -> Vec<String> {
    origins
        .iter()
        .filter_map(|origin| origin.parse::<axum::http::Uri>().ok())
        .filter(|uri| matches!(uri.scheme_str(), Some("http" | "https")))
        .filter(|uri| matches!(uri.host(), Some("localhost" | "127.0.0.1" | "[::1]")))
        .filter_map(|uri| {
            uri.authority()
                .map(|authority| authority.as_str().to_owned())
        })
        .collect()
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

#[cfg(test)]
mod local_host_tests {
    #[test]
    fn configured_origins_deduplicate_seeded_hosts_for_default_and_mixed_ports() {
        let hosts = super::local_development_hosts(&[
            "http://127.0.0.1:5173".to_owned(),
            "http://127.0.0.1:8080".to_owned(),
            "http://127.0.0.1:15173".to_owned(),
            "http://127.0.0.1:8080".to_owned(),
            "http://localhost:18080".to_owned(),
        ]);
        assert_eq!(
            hosts,
            [
                "localhost",
                "localhost:5173",
                "localhost:8080",
                "127.0.0.1",
                "127.0.0.1:5173",
                "127.0.0.1:8080",
                "127.0.0.1:15173",
                "localhost:18080"
            ]
        );
    }

    #[test]
    fn isolated_development_ports_only_extend_loopback_identity_hosts() {
        assert_eq!(
            super::local_origin_hosts(&[
                "http://127.0.0.1:15173".to_owned(),
                "https://localhost:18080".to_owned(),
                "https://tenant.example.invalid".to_owned(),
                "invalid".to_owned(),
            ]),
            ["127.0.0.1:15173", "localhost:18080"]
        );
    }
}
