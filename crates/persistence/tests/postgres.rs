use std::sync::Arc;
use std::time::Duration;

use geo_domain::{
    AgentRepository, AppendMessage, AttachmentId, AttachmentReference, CreateConversation,
    ErrorCode, InitialSource, InitialSourceKind, InitialSourceVisibility, MessageRole, ObjectRef,
    ProjectCreate, ProjectRepository, ProjectSettings, ProjectStartCommand, RecordToolCall,
    RunCompletion, RunStatus, RuntimeCapability, StoreCheckpoint, TenantScope, ToolCallDecision,
    ToolCallOutcome, TurnStatus, hash_idempotency_key, settings_hash, start_request_hash,
};
use geo_persistence::{Database, DatabaseConfig, PgAgentRepository, PgProjectRepository};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn embedded_migrations_apply_to_postgres_when_configured() {
    let database_url =
        std::env::var("GEO_TEST_DATABASE_URL").expect("GEO_TEST_DATABASE_URL is required");

    let config = DatabaseConfig::from_url(database_url).expect("valid test database URL");
    let database = Database::connect_and_migrate(&config)
        .await
        .expect("connect and apply embedded migrations");

    // Running the embedded migration set again must be safe.
    database.migrate().await.expect("re-run migration set");

    let required_tables = [
        "operators",
        "tenants",
        "projects",
        "operations",
        "idempotency_records",
        "outbox_events",
        "project_config_revisions",
        "project_start_records",
        "optimization_cycles",
        "document_manifests",
        "distribution_manifests",
        "workflow_runs",
        "agent_conversations",
        "agent_messages",
        "agent_message_attachments",
        "agent_turns",
        "agent_runs",
        "agent_checkpoints",
        "agent_tool_call_ledger",
        "agent_conversation_events",
        "agent_submissions",
    ];

    for table in required_tables {
        let relation: Option<String> = sqlx::query_scalar("SELECT to_regclass($1)::text")
            .bind(format!("public.{table}"))
            .fetch_one(database.pool())
            .await
            .expect("query migration result");
        assert_eq!(relation.as_deref(), Some(table));
    }
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn atomic_start_and_scope_visibility_hold_when_postgres_is_configured() {
    let database_url =
        std::env::var("GEO_TEST_DATABASE_URL").expect("GEO_TEST_DATABASE_URL is required");
    let config = DatabaseConfig::from_url(database_url).expect("valid test database URL");
    let database = Database::connect_and_migrate(&config)
        .await
        .expect("migrations");
    let operator_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();
    let other_tenant_id = Uuid::new_v4();
    sqlx::query("INSERT INTO operators (operator_id, slug, display_name) VALUES ($1,$2,$3)")
        .bind(operator_id)
        .bind(format!("atomic-{operator_id}"))
        .bind("Atomic test operator")
        .execute(database.pool())
        .await
        .expect("operator");
    for tenant_id in [tenant_id, other_tenant_id] {
        sqlx::query(
            "INSERT INTO tenants (tenant_id, operator_id, slug, display_name) VALUES ($1,$2,$3,$4)",
        )
        .bind(tenant_id)
        .bind(operator_id)
        .bind(format!("tenant-{tenant_id}"))
        .bind("Atomic test tenant")
        .execute(database.pool())
        .await
        .expect("tenant");
    }
    let scope = TenantScope::new(operator_id.into(), tenant_id.into(), None);
    let repository = PgProjectRepository::from_database(&database);
    let project = repository
        .create(
            &scope,
            ProjectCreate {
                slug: Some(format!("atomic-{tenant_id}")),
                display_name: "Atomic start".to_owned(),
                settings: ProjectSettings {
                    brand_name: "Acme".to_owned(),
                    market: "US".to_owned(),
                    language: "en".to_owned(),
                    initial_sources: vec![InitialSource {
                        kind: InitialSourceKind::Url,
                        value: "https://example.com".to_owned(),
                        visibility: InitialSourceVisibility::Public,
                        version_ref: None,
                        content_hash: None,
                    }],
                    ..ProjectSettings::default()
                },
            },
        )
        .await
        .expect("draft");
    let frozen = project
        .settings
        .clone()
        .validate_start()
        .expect("startable");
    let frozen_hash = settings_hash(&frozen).expect("settings hash");
    let command = ProjectStartCommand {
        expected_revision: project.revision,
        idempotency_key_hash: hash_idempotency_key("atomic-start"),
        request_hash: start_request_hash(project.id, project.revision, &frozen_hash),
        settings_hash: frozen_hash,
        operation_id: Uuid::new_v4(),
    };
    let acceptance = repository
        .start(&scope, project.id, command.clone())
        .await
        .expect("atomic start");
    assert!(!acceptance.document_manifest.sealed);
    assert_eq!(acceptance.document_manifest.expected_count, None);
    assert_eq!(acceptance.document_manifest.state, "awaiting_knowledge");
    assert_eq!(acceptance.distribution_manifest.state, "awaiting_documents");
    assert_eq!(
        repository
            .start(&scope, project.id, command)
            .await
            .expect("same-key replay"),
        acceptance
    );
    let other_scope = TenantScope::new(operator_id.into(), other_tenant_id.into(), None);
    assert!(
        repository
            .get_start(&other_scope, project.id)
            .await
            .expect("scoped lookup")
            .is_none()
    );
}

async fn connect() -> Database {
    let database_url =
        std::env::var("GEO_TEST_DATABASE_URL").expect("GEO_TEST_DATABASE_URL is required");
    let config = DatabaseConfig::from_url(database_url).expect("valid test database URL");
    Database::connect_and_migrate(&config)
        .await
        .expect("connect and apply embedded migrations")
}

/// Creates an operator, a tenant and one project, and returns the project scope.
async fn seed_scope(pool: &PgPool, label: &str) -> TenantScope {
    let operator_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();
    sqlx::query("INSERT INTO operators (operator_id, slug, display_name) VALUES ($1,$2,$3)")
        .bind(operator_id)
        .bind(format!("agent-{label}-{operator_id}"))
        .bind("Agent test operator")
        .execute(pool)
        .await
        .expect("operator");
    sqlx::query(
        "INSERT INTO tenants (tenant_id, operator_id, slug, display_name) VALUES ($1,$2,$3,$4)",
    )
    .bind(tenant_id)
    .bind(operator_id)
    .bind(format!("agent-{label}-{tenant_id}"))
    .bind("Agent test tenant")
    .execute(pool)
    .await
    .expect("tenant");
    TenantScope::new(
        operator_id.into(),
        tenant_id.into(),
        Some(
            seed_project(pool, operator_id, tenant_id, label)
                .await
                .into(),
        ),
    )
}

/// Adds a second project inside the same operator/tenant as `scope`.
async fn seed_sibling_scope(pool: &PgPool, scope: &TenantScope, label: &str) -> TenantScope {
    TenantScope::new(
        scope.operator_id,
        scope.tenant_id,
        Some(
            seed_project(
                pool,
                scope.operator_id.as_uuid(),
                scope.tenant_id.as_uuid(),
                label,
            )
            .await
            .into(),
        ),
    )
}

async fn seed_project(pool: &PgPool, operator_id: Uuid, tenant_id: Uuid, label: &str) -> Uuid {
    let project_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO projects (project_id, operator_id, tenant_id, slug, display_name, status)
         VALUES ($1,$2,$3,$4,$5,'active')",
    )
    .bind(project_id)
    .bind(operator_id)
    .bind(tenant_id)
    .bind(format!("agent-{label}-{project_id}"))
    .bind("Agent test project")
    .execute(pool)
    .await
    .expect("project");
    project_id
}

fn message(content: &str) -> AppendMessage {
    AppendMessage {
        content: content.to_owned(),
        attachments: Vec::new(),
        metadata: Value::Null,
    }
}

async fn create_conversation(
    repository: &PgAgentRepository,
    scope: &TenantScope,
) -> geo_domain::Conversation {
    repository
        .create_conversation(scope, None, CreateConversation::default())
        .await
        .expect("create conversation")
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_conversations_are_isolated_by_tenant_and_project() {
    let database = connect().await;
    let repository = PgAgentRepository::from_database(&database);
    let scope = seed_scope(database.pool(), "isolation").await;
    let conversation = create_conversation(&repository, &scope).await;

    assert_eq!(
        repository
            .list_conversations(&scope)
            .await
            .expect("scoped list")
            .len(),
        1
    );
    assert!(
        repository
            .get_conversation(&scope, conversation.id)
            .await
            .expect("scoped read")
            .is_some()
    );

    let sibling = seed_sibling_scope(database.pool(), &scope, "isolation-sibling").await;
    assert!(
        repository
            .list_conversations(&sibling)
            .await
            .expect("sibling list")
            .is_empty()
    );
    assert!(
        repository
            .get_conversation(&sibling, conversation.id)
            .await
            .expect("sibling read")
            .is_none()
    );
    assert_eq!(
        repository
            .replay_events(&sibling, conversation.id, Some(0))
            .await
            .expect_err("sibling replay")
            .code,
        ErrorCode::NotFound
    );

    let other_tenant = seed_scope(database.pool(), "isolation-tenant").await;
    assert!(
        repository
            .list_conversations(&other_tenant)
            .await
            .expect("other tenant list")
            .is_empty()
    );
    assert!(
        repository
            .get_conversation(&other_tenant, conversation.id)
            .await
            .expect("other tenant read")
            .is_none()
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_message_submission_is_idempotent_and_rejects_a_changed_request() {
    let database = connect().await;
    let repository = PgAgentRepository::from_database(&database);
    let scope = seed_scope(database.pool(), "idempotency").await;
    let conversation = create_conversation(&repository, &scope).await;
    let capability = RuntimeCapability::missing("runtime missing");

    let first = repository
        .append_message(
            &scope,
            conversation.id,
            message("hello"),
            "same-key".to_owned(),
            "body-a".to_owned(),
            capability.clone(),
        )
        .await
        .expect("first submission");
    let replay = repository
        .append_message(
            &scope,
            conversation.id,
            message("hello"),
            "same-key".to_owned(),
            "body-a".to_owned(),
            capability.clone(),
        )
        .await
        .expect("same-key replay");
    assert_eq!(first, replay);
    assert_eq!(replay.message.id, first.message.id);

    let conflict = repository
        .append_message(
            &scope,
            conversation.id,
            message("different"),
            "same-key".to_owned(),
            "body-b".to_owned(),
            capability,
        )
        .await
        .expect_err("same key with a different request");
    assert_eq!(conflict.code, ErrorCode::Conflict);

    // The replay returned the stored acceptance instead of writing new rows.
    let detail = repository
        .get_conversation(&scope, conversation.id)
        .await
        .expect("detail")
        .expect("present");
    assert_eq!(detail.messages.len(), 1);
    assert_eq!(detail.turns.len(), 1);
    assert_eq!(detail.runs.len(), 1);
    let events = repository
        .replay_events(&scope, conversation.id, Some(0))
        .await
        .expect("events");
    assert_eq!(events.len(), 4);
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_run_state_machine_fails_without_a_runtime_and_cancels_with_one() {
    let database = connect().await;
    let repository = PgAgentRepository::from_database(&database);
    let scope = seed_scope(database.pool(), "state-machine").await;
    let conversation = create_conversation(&repository, &scope).await;

    let failed = repository
        .append_message(
            &scope,
            conversation.id,
            message("hello"),
            "failed-key".to_owned(),
            "failed-body".to_owned(),
            RuntimeCapability::missing("runtime missing"),
        )
        .await
        .expect("submission without a runtime");
    assert_eq!(failed.turn.status, TurnStatus::Failed);
    assert_eq!(failed.run.status, RunStatus::Failed);
    assert_eq!(
        failed.run.error.as_ref().map(|error| error.code),
        Some(ErrorCode::CapabilityMissing)
    );
    let events = repository
        .replay_events(&scope, conversation.id, Some(0))
        .await
        .expect("events");
    assert!(events.iter().any(|event| event.event_type == "run.failed"));
    assert!(
        events
            .windows(2)
            .all(|window| window[0].sequence < window[1].sequence)
    );

    // A failed run releases the conversation, so the next turn is accepted.
    let queued = repository
        .append_message(
            &scope,
            conversation.id,
            message("again"),
            "queued-key".to_owned(),
            "queued-body".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("submission with a runtime");
    assert_eq!(queued.turn.status, TurnStatus::Queued);
    assert_eq!(queued.run.status, RunStatus::Queued);

    // Only one turn may be active per conversation.
    let concurrent = repository
        .append_message(
            &scope,
            conversation.id,
            message("too soon"),
            "concurrent-key".to_owned(),
            "concurrent-body".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect_err("active turn");
    assert_eq!(concurrent.code, ErrorCode::Conflict);

    let cancelled = repository
        .cancel_turn(&scope, queued.turn.id)
        .await
        .expect("cancel");
    assert_eq!(cancelled.status, RunStatus::Cancelled);
    assert_eq!(cancelled.cancel_version, 1);
    let cancelled_again = repository
        .cancel_turn(&scope, queued.turn.id)
        .await
        .expect("cancel is idempotent");
    assert_eq!(cancelled_again.cancel_version, 1);
    assert_eq!(cancelled_again.status, RunStatus::Cancelled);

    let detail = repository
        .get_conversation(&scope, conversation.id)
        .await
        .expect("detail")
        .expect("present");
    assert_eq!(detail.turns.len(), 2);
    assert_eq!(detail.turns[1].status, TurnStatus::Cancelled);
    assert_eq!(detail.runs[1].status, RunStatus::Cancelled);
    assert_eq!(
        detail
            .turns
            .iter()
            .filter(|turn| matches!(turn.status, TurnStatus::Queued | TurnStatus::Running))
            .count(),
        0
    );

    let sibling = seed_sibling_scope(database.pool(), &scope, "state-machine-sibling").await;
    assert_eq!(
        repository
            .cancel_turn(&sibling, queued.turn.id)
            .await
            .expect_err("cross-project cancel")
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        repository
            .cancel_turn(&scope, geo_domain::TurnId::from(Uuid::new_v4()))
            .await
            .expect_err("unknown turn")
            .code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_event_sequence_is_monotonic_under_concurrent_writers() {
    let database = connect().await;
    let pool = database.pool().clone();
    let repository = Arc::new(PgAgentRepository::new(pool.clone()));
    let scope = seed_scope(&pool, "concurrent-events").await;
    let conversation = create_conversation(&repository, &scope).await;

    let concurrency = 5usize;
    let mut handles = Vec::with_capacity(concurrency);
    for index in 0..concurrency {
        let repository = Arc::clone(&repository);
        let scope = scope.clone();
        let conversation_id = conversation.id;
        handles.push(tokio::spawn(async move {
            repository
                .append_message(
                    &scope,
                    conversation_id,
                    message(&format!("message {index}")),
                    format!("concurrent-key-{index}"),
                    format!("concurrent-body-{index}"),
                    RuntimeCapability::missing("runtime missing"),
                )
                .await
        }));
    }
    for handle in handles {
        handle
            .await
            .expect("join append")
            .expect("concurrent append");
    }

    let events = repository
        .replay_events(&scope, conversation.id, Some(0))
        .await
        .expect("events");
    let expected_events = 1 + 3 * u64::try_from(concurrency).expect("small concurrency");
    assert_eq!(events.len() as u64, expected_events);
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        (1..=expected_events).collect::<Vec<_>>()
    );

    let detail = repository
        .get_conversation(&scope, conversation.id)
        .await
        .expect("detail")
        .expect("present");
    let expected_messages = u64::try_from(concurrency).expect("small concurrency");
    assert_eq!(detail.messages.len(), concurrency);
    assert_eq!(
        detail
            .messages
            .iter()
            .map(|message| message.sequence)
            .collect::<Vec<_>>(),
        (1..=expected_messages).collect::<Vec<_>>()
    );
    assert_eq!(detail.turns.len(), concurrency);
    assert_eq!(
        detail
            .turns
            .iter()
            .filter(|turn| turn.previous_turn_id.is_none())
            .count(),
        1
    );
    for turn in &detail.turns {
        if let Some(previous) = turn.previous_turn_id {
            assert_ne!(previous, turn.id);
            assert!(
                detail
                    .turns
                    .iter()
                    .any(|candidate| candidate.id == previous)
            );
        }
    }
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_replay_after_restart_returns_the_durable_events() {
    let database = connect().await;
    let pool = database.pool().clone();
    let repository = PgAgentRepository::new(pool.clone());
    let scope = seed_scope(&pool, "replay").await;
    let conversation = create_conversation(&repository, &scope).await;
    let acceptance = repository
        .append_message(
            &scope,
            conversation.id,
            message("hello"),
            "replay-key".to_owned(),
            "replay-body".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("append message");
    let before_restart = repository
        .replay_events(&scope, conversation.id, Some(0))
        .await
        .expect("events before restart");

    // A new process only has the pool; nothing is carried over in memory.
    let restarted = PgAgentRepository::new(pool.clone());
    let after_restart = restarted
        .replay_events(&scope, conversation.id, Some(0))
        .await
        .expect("events after restart");
    assert_eq!(after_restart, before_restart);
    assert_eq!(after_restart.len(), 3);
    assert_eq!(after_restart[0].event_type, "conversation.created");
    assert_eq!(after_restart[2].event_type, "turn.accepted");

    let cursor = after_restart[0].sequence;
    let incremental = restarted
        .replay_events(&scope, conversation.id, Some(cursor))
        .await
        .expect("incremental replay");
    assert_eq!(incremental, after_restart[1..].to_vec());

    let replayed_submission = restarted
        .append_message(
            &scope,
            conversation.id,
            message("hello"),
            "replay-key".to_owned(),
            "replay-body".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("idempotent replay after restart");
    assert_eq!(replayed_submission, acceptance);
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_checkpoints_survive_a_restart_and_reject_a_changed_input() {
    let database = connect().await;
    let pool = database.pool().clone();
    let repository = PgAgentRepository::new(pool.clone());
    let scope = seed_scope(&pool, "checkpoint").await;
    let conversation = create_conversation(&repository, &scope).await;
    let acceptance = repository
        .append_message(
            &scope,
            conversation.id,
            message("hello"),
            "checkpoint-key".to_owned(),
            "checkpoint-body".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("append message");
    let run_id = acceptance.run.id;
    let checkpoint = |input_hash: &str, cursor: i64| StoreCheckpoint {
        checkpoint_scope: "loop".to_owned(),
        step_key: "collect".to_owned(),
        input_hash: input_hash.to_owned(),
        result_ref: Some(ObjectRef {
            object_id: "object-1".to_owned(),
            version: Some("3".to_owned()),
        }),
        state: json!({"cursor": cursor}),
    };

    let stored = repository
        .store_checkpoint(&scope, run_id, checkpoint("digest-a", 3))
        .await
        .expect("store checkpoint");
    assert_eq!(stored.version, 1);
    assert_eq!(stored.run_id, run_id);
    assert_eq!(stored.conversation_id, conversation.id);

    let restarted = PgAgentRepository::new(pool.clone());
    let restored = restarted
        .load_checkpoint(&scope, run_id, "loop", "collect")
        .await
        .expect("load checkpoint")
        .expect("checkpoint is durable");
    assert_eq!(restored, stored);

    let refreshed = restarted
        .store_checkpoint(&scope, run_id, checkpoint("digest-a", 4))
        .await
        .expect("refresh checkpoint");
    assert_eq!(refreshed.id, stored.id);
    assert_eq!(refreshed.version, 2);
    assert_eq!(refreshed.state, json!({"cursor": 4}));

    let conflict = restarted
        .store_checkpoint(&scope, run_id, checkpoint("digest-b", 5))
        .await
        .expect_err("changed input digest");
    assert_eq!(conflict.code, ErrorCode::Conflict);

    assert!(
        restarted
            .load_checkpoint(&scope, run_id, "loop", "missing-step")
            .await
            .expect("missing step")
            .is_none()
    );
    let sibling = seed_sibling_scope(&pool, &scope, "checkpoint-sibling").await;
    assert_eq!(
        restarted
            .load_checkpoint(&sibling, run_id, "loop", "collect")
            .await
            .expect_err("cross-project checkpoint")
            .code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_tool_call_ledger_appends_idempotently_per_tool_call() {
    let database = connect().await;
    let pool = database.pool().clone();
    let repository = PgAgentRepository::new(pool.clone());
    let scope = seed_scope(&pool, "ledger").await;
    let conversation = create_conversation(&repository, &scope).await;
    let acceptance = repository
        .append_message(
            &scope,
            conversation.id,
            message("hello"),
            "ledger-key".to_owned(),
            "ledger-body".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("append message");
    let run_id = acceptance.run.id;
    let record = |tool_call_id: &str, arguments_hash: &str| RecordToolCall {
        run_id,
        tool_call_id: tool_call_id.to_owned(),
        tool_name: "geo.publish".to_owned(),
        arguments_hash: arguments_hash.to_owned(),
        idempotency_key_hash: "ledger-idempotency".to_owned(),
        permission: ToolCallDecision::Allowed,
        budget: ToolCallDecision::Allowed,
        intent: json!({"document_id": "doc-1"}),
        attempt_count: 0,
        result_ref: Some(ObjectRef {
            object_id: "object-2".to_owned(),
            version: None,
        }),
        outcome: ToolCallOutcome::Intent,
        cost_minor: Some(12),
        currency: Some("CNY".to_owned()),
    };

    let appended = repository
        .append_tool_call(&scope, record("call-1", "args-a"))
        .await
        .expect("append tool call");
    assert_eq!(appended.run_id, run_id);
    assert_eq!(appended.turn_id, acceptance.turn.id);
    assert_eq!(appended.conversation_id, conversation.id);
    assert_eq!(appended.idempotency_key_hash, "ledger-idempotency");

    let replayed = repository
        .append_tool_call(&scope, record("call-1", "args-a"))
        .await
        .expect("replay tool call");
    assert_eq!(replayed, appended);

    let conflict = repository
        .append_tool_call(&scope, record("call-1", "args-b"))
        .await
        .expect_err("different arguments");
    assert_eq!(conflict.code, ErrorCode::Conflict);

    let listed = PgAgentRepository::new(pool.clone())
        .list_tool_calls(&scope, run_id)
        .await
        .expect("list tool calls");
    assert_eq!(listed, vec![appended.clone()]);
    assert_eq!(listed[0].attempt_count, 0);
    assert_eq!(listed[0].cost_minor, Some(12));
    assert_eq!(listed[0].currency.as_deref(), Some("CNY"));

    let sibling = seed_sibling_scope(&pool, &scope, "ledger-sibling").await;
    assert_eq!(
        repository
            .list_tool_calls(&sibling, run_id)
            .await
            .expect_err("cross-project list")
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        repository
            .append_tool_call(&sibling, record("call-2", "args-a"))
            .await
            .expect_err("cross-project append")
            .code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_attachment_only_message_round_trips_and_links_the_previous_turn() {
    let database = connect().await;
    let pool = database.pool().clone();
    let repository = PgAgentRepository::new(pool.clone());
    let scope = seed_scope(&pool, "attachments").await;
    let conversation = create_conversation(&repository, &scope).await;
    let first = repository
        .append_message(
            &scope,
            conversation.id,
            message("hello"),
            "attachment-first".to_owned(),
            "attachment-first-body".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("first message");

    let attachment = AttachmentReference {
        attachment_id: AttachmentId::from(Uuid::new_v4()),
        object_id: "object-attachment-1".to_owned(),
        filename: "report.pdf".to_owned(),
        media_type: Some("application/pdf".to_owned()),
        size_bytes: Some(2048),
        sha256: Some("a".repeat(64)),
        object_version: Some("2".to_owned()),
    };
    // An attachment-only message would be accepted, but the first turn is still
    // active, so the conflict has to win before the attachment rules are tested.
    let active = repository
        .append_message(
            &scope,
            conversation.id,
            AppendMessage {
                content: "  ".to_owned(),
                attachments: vec![attachment.clone()],
                metadata: Value::Null,
            },
            "attachment-second".to_owned(),
            "attachment-second-body".to_owned(),
            RuntimeCapability::missing("runtime missing"),
        )
        .await
        .expect_err("active turn");
    assert_eq!(active.code, ErrorCode::Conflict);

    repository
        .cancel_turn(&scope, first.turn.id)
        .await
        .expect("cancel first turn");
    let second = repository
        .append_message(
            &scope,
            conversation.id,
            AppendMessage {
                content: "  ".to_owned(),
                attachments: vec![attachment.clone()],
                metadata: json!({"source": "upload"}),
            },
            "attachment-second".to_owned(),
            "attachment-second-body".to_owned(),
            RuntimeCapability::missing("runtime missing"),
        )
        .await
        .expect("attachment-only message");
    assert_eq!(second.message.content, "");
    assert_eq!(second.message.attachments, vec![attachment.clone()]);
    assert_eq!(second.message.sequence, 2);
    assert_eq!(second.turn.previous_turn_id, Some(first.turn.id));
    assert_eq!(second.turn.root_message_id, second.message.id);

    let restarted = PgAgentRepository::new(pool.clone());
    let detail = restarted
        .get_conversation(&scope, conversation.id)
        .await
        .expect("detail")
        .expect("present");
    assert_eq!(detail.messages.len(), 2);
    let restored = &detail.messages[1];
    assert_eq!(restored.content, "");
    assert_eq!(restored.attachments, vec![attachment]);
    assert_eq!(restored.turn_id, Some(second.turn.id));
    assert_eq!(restored.metadata, json!({"source": "upload"}));
    assert_eq!(restored.sequence, 2);
    assert_eq!(detail.turns.len(), 2);
    assert_eq!(detail.turns[1].previous_turn_id, Some(first.turn.id));
    assert_eq!(detail.runs.len(), 2);
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_cancel_racing_completion_keeps_a_single_terminal_state() {
    let database = connect().await;
    let pool = database.pool().clone();
    let repository = PgAgentRepository::new(pool.clone());
    let scope = seed_scope(&pool, "cancel-race").await;
    let conversation = create_conversation(&repository, &scope).await;
    let acceptance = repository
        .append_message(
            &scope,
            conversation.id,
            message("hello"),
            "cancel-race-key".to_owned(),
            "cancel-race-body".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("append message");
    let run_id = acceptance.run.id;
    let turn_id = acceptance.turn.id;

    // A worker holding the run row finishes only while the run is still open;
    // the same lock is what cancel takes, so exactly one of them can decide.
    let completion = tokio::spawn(async move {
        let mut transaction = pool.begin().await.expect("completion transaction");
        let status: String =
            sqlx::query_scalar("SELECT status FROM agent_runs WHERE run_id = $1 FOR UPDATE")
                .bind(run_id.as_uuid())
                .fetch_one(&mut *transaction)
                .await
                .expect("lock run");
        if status != "queued" && status != "running" {
            transaction.rollback().await.expect("rollback completion");
            return;
        }
        sqlx::query(
            "UPDATE agent_runs SET status = 'succeeded', updated_at = now() WHERE run_id = $1",
        )
        .bind(run_id.as_uuid())
        .execute(&mut *transaction)
        .await
        .expect("complete run");
        sqlx::query(
            "UPDATE agent_turns SET status = 'succeeded', updated_at = now() WHERE turn_id = $1",
        )
        .bind(turn_id.as_uuid())
        .execute(&mut *transaction)
        .await
        .expect("complete turn");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        transaction.commit().await.expect("commit completion");
    });

    let cancelled = repository
        .cancel_turn(&scope, turn_id)
        .await
        .expect("cancel turn");
    completion.await.expect("join completion");

    let detail = repository
        .get_conversation(&scope, conversation.id)
        .await
        .expect("detail")
        .expect("present");
    let run = detail
        .runs
        .iter()
        .find(|run| run.id == run_id)
        .expect("run");
    let turn = detail
        .turns
        .iter()
        .find(|turn| turn.id == turn_id)
        .expect("turn");
    assert!(matches!(
        run.status,
        RunStatus::Succeeded | RunStatus::Cancelled
    ));
    assert_eq!(run.status, cancelled.status);
    assert_eq!(
        turn.status == TurnStatus::Succeeded,
        run.status == RunStatus::Succeeded
    );

    let events = repository
        .replay_events(&scope, conversation.id, Some(0))
        .await
        .expect("events");
    let cancelled_events = events
        .iter()
        .filter(|event| event.event_type == "run.cancelled")
        .count();
    assert_eq!(
        cancelled_events,
        usize::from(run.status == RunStatus::Cancelled)
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_concurrent_cancels_emit_one_cancellation() {
    let database = connect().await;
    let pool = database.pool().clone();
    let repository = Arc::new(PgAgentRepository::new(pool.clone()));
    let scope = seed_scope(&pool, "cancel-concurrent").await;
    let conversation = create_conversation(&repository, &scope).await;
    let acceptance = repository
        .append_message(
            &scope,
            conversation.id,
            message("hello"),
            "cancel-concurrent-key".to_owned(),
            "cancel-concurrent-body".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("append message");

    let mut handles = Vec::new();
    for _ in 0..3 {
        let repository = Arc::clone(&repository);
        let scope = scope.clone();
        let turn_id = acceptance.turn.id;
        handles.push(tokio::spawn(async move {
            repository.cancel_turn(&scope, turn_id).await
        }));
    }
    for handle in handles {
        let run = handle.await.expect("join cancel").expect("cancel turn");
        assert_eq!(run.status, RunStatus::Cancelled);
        assert_eq!(run.cancel_version, 1);
    }

    let events = repository
        .replay_events(&scope, conversation.id, Some(0))
        .await
        .expect("events");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "run.cancelled")
            .count(),
        1
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_begin_run_claims_a_queued_run_exactly_once() {
    let database = connect().await;
    let pool = database.pool().clone();
    let repository = Arc::new(PgAgentRepository::new(pool.clone()));
    let scope = seed_scope(&pool, "claim").await;
    let conversation = create_conversation(&repository, &scope).await;
    let acceptance = repository
        .append_message(
            &scope,
            conversation.id,
            message("hello"),
            "claim-key".to_owned(),
            "claim-body".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("append message");
    let run_id = acceptance.run.id;

    // Several workers race for the same run, the way a replayed dispatch would.
    let mut handles = Vec::new();
    for _ in 0..4 {
        let repository = Arc::clone(&repository);
        let scope = scope.clone();
        handles.push(tokio::spawn(async move {
            repository.begin_run(&scope, run_id).await
        }));
    }
    let mut claims = Vec::new();
    for handle in handles {
        claims.push(handle.await.expect("join claim").expect("begin run"));
    }
    let claimed: Vec<_> = claims.iter().flatten().collect();
    assert_eq!(
        claimed.len(),
        1,
        "exactly one caller may own the run: {claims:?}"
    );
    assert_eq!(claimed[0].status, RunStatus::Running);

    // A run is not claimable twice, and not from outside its own scope.
    assert!(
        repository
            .begin_run(&scope, run_id)
            .await
            .expect("second claim")
            .is_none(),
        "a claimed run is not claimable again"
    );
    let sibling = seed_sibling_scope(&pool, &scope, "claim").await;
    assert!(
        repository
            .begin_run(&sibling, run_id)
            .await
            .expect("cross-scope claim")
            .is_none(),
        "a run must not be claimable from a sibling project"
    );

    // A cancelled run is not claimable either: the cancellation outranks a
    // worker that has not started yet.  It needs a conversation of its own,
    // because the first one still has an active turn.
    let second = create_conversation(&repository, &scope).await;
    let cancelling = repository
        .append_message(
            &scope,
            second.id,
            message("second"),
            "claim-key-2".to_owned(),
            "claim-body-2".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("append second message");
    repository
        .cancel_turn(&scope, cancelling.turn.id)
        .await
        .expect("cancel second turn");
    assert!(
        repository
            .begin_run(&scope, cancelling.run.id)
            .await
            .expect("claim after cancel")
            .is_none(),
        "a cancelled run must not be claimed for execution"
    );

    let events = repository
        .replay_events(&scope, conversation.id, Some(0))
        .await
        .expect("events");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "run.running")
            .count(),
        1,
        "only the winning claim may be recorded"
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_finish_run_records_the_answer_once_and_never_over_a_cancellation() {
    let database = connect().await;
    let pool = database.pool().clone();
    let repository = Arc::new(PgAgentRepository::new(pool.clone()));
    let scope = seed_scope(&pool, "finish").await;
    let conversation = create_conversation(&repository, &scope).await;

    // The happy path, driven through the real claim rather than around it.
    let acceptance = repository
        .append_message(
            &scope,
            conversation.id,
            message("how long is the warranty?"),
            "finish-key".to_owned(),
            "finish-body".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("append message");
    repository
        .begin_run(&scope, acceptance.run.id)
        .await
        .expect("begin run")
        .expect("the run must be claimable");
    let transition = repository
        .finish_run(
            &scope,
            acceptance.run.id,
            RunCompletion::Succeeded {
                content: "twenty-four months".to_owned(),
                metadata: json!({"source": "test"}),
            },
        )
        .await
        .expect("finish run")
        .expect("the run must have been running");
    assert_eq!(transition.run.status, RunStatus::Succeeded);
    assert_eq!(transition.turn.status, TurnStatus::Succeeded);
    let answer = transition
        .message
        .expect("a succeeded run carries its answer");
    assert_eq!(answer.role, MessageRole::Assistant);
    assert_eq!(answer.content, "twenty-four months");

    // Finishing twice must not add a second answer.
    assert!(
        repository
            .finish_run(
                &scope,
                acceptance.run.id,
                RunCompletion::Succeeded {
                    content: "a second answer".to_owned(),
                    metadata: Value::Null,
                },
            )
            .await
            .expect("second finish")
            .is_none(),
        "a terminal run must not be finished again"
    );

    // An answer that cannot be stored fails the run instead of being truncated.
    let unstorable = repository
        .append_message(
            &scope,
            conversation.id,
            message("empty answer please"),
            "finish-key-empty".to_owned(),
            "finish-body-empty".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("append message");
    repository
        .begin_run(&scope, unstorable.run.id)
        .await
        .expect("begin run")
        .expect("claimable");
    let failed = repository
        .finish_run(
            &scope,
            unstorable.run.id,
            RunCompletion::Succeeded {
                content: "   ".to_owned(),
                metadata: Value::Null,
            },
        )
        .await
        .expect("finish run")
        .expect("running");
    assert_eq!(failed.run.status, RunStatus::Failed);
    assert!(
        failed.message.is_none(),
        "an unstorable answer is not stored"
    );
    assert_eq!(
        failed.run.error.as_ref().map(|error| error.code),
        Some(ErrorCode::InvalidRequest)
    );

    // Cancellation wins over a completion, and the answer is discarded.
    let raced = repository
        .append_message(
            &scope,
            conversation.id,
            message("cancel me"),
            "finish-key-race".to_owned(),
            "finish-body-race".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("append message");
    repository
        .begin_run(&scope, raced.run.id)
        .await
        .expect("begin run")
        .expect("claimable");
    repository
        .cancel_turn(&scope, raced.turn.id)
        .await
        .expect("cancel turn");
    assert!(
        repository
            .finish_run(
                &scope,
                raced.run.id,
                RunCompletion::Succeeded {
                    content: "too late".to_owned(),
                    metadata: Value::Null,
                },
            )
            .await
            .expect("late finish")
            .is_none(),
        "a cancelled run must not accept a completion"
    );

    let detail = repository
        .get_conversation(&scope, conversation.id)
        .await
        .expect("detail")
        .expect("present");
    let answers: Vec<_> = detail
        .messages
        .iter()
        .filter(|message| message.role == MessageRole::Assistant)
        .collect();
    assert_eq!(
        answers.len(),
        1,
        "exactly one answer must be durable: {answers:?}"
    );
    assert_eq!(answers[0].content, "twenty-four months");
    let raced_run = detail
        .runs
        .iter()
        .find(|run| run.id == raced.run.id)
        .expect("raced run");
    assert_eq!(raced_run.status, RunStatus::Cancelled);

    let events = repository
        .replay_events(&scope, conversation.id, Some(0))
        .await
        .expect("events");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "run.succeeded")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "run.failed")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "run.cancelled")
            .count(),
        1
    );
}

#[tokio::test]
async fn agent_unreachable_database_fails_closed() {
    // No credentials or database are required here: the pool targets a closed
    // local port, so the repository has to report a dependency failure instead
    // of silently serving process memory.
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_millis(250))
        .connect_lazy("postgres://geo:geo@127.0.0.1:1/geo_test")
        .expect("valid lazy pool");
    let repository = PgAgentRepository::new(pool);
    let scope = TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    );

    for error in [
        repository
            .list_conversations(&scope)
            .await
            .expect_err("list"),
        repository
            .create_conversation(&scope, None, CreateConversation::default())
            .await
            .expect_err("create"),
        repository
            .replay_events(&scope, Uuid::new_v4().into(), Some(0))
            .await
            .expect_err("replay"),
        repository
            .get_conversation(&scope, Uuid::new_v4().into())
            .await
            .expect_err("read"),
    ] {
        assert_eq!(error.code, ErrorCode::DependencyUnavailable);
    }
}
