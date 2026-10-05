//! Run explicitly against a disposable database via GEO_TEST_DATABASE_URL.
use chrono::Utc;
use geo_domain::{
    ChannelJobRepository, ChannelOutcome, ChannelOutcomeStatus, ChannelPlan, ChannelSecret,
    ChannelTarget, ChannelTargetInput, ErrorCode, InitialSource, InitialSourceKind,
    InitialSourceVisibility, ProjectCreate, ProjectRepository, ProjectSettings,
    ProjectStartCommand, TenantScope, hash_idempotency_key, settings_hash, start_request_hash,
};
use geo_persistence::{Database, PgChannelJobRepository, PgProjectRepository};
use uuid::Uuid;

async fn scope_with_cycle(database: &Database) -> (TenantScope, Uuid) {
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    sqlx::query("INSERT INTO operators(operator_id,slug,display_name) VALUES($1,$2,'Fixture')")
        .bind(operator)
        .bind(format!("binding-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants(tenant_id,operator_id,slug,display_name) VALUES($1,$2,$3,'Fixture')",
    )
    .bind(tenant)
    .bind(operator)
    .bind(format!("binding-{tenant}"))
    .execute(database.pool())
    .await
    .unwrap();
    let owner = TenantScope::new(operator.into(), tenant.into(), None);
    let projects = PgProjectRepository::from_database(database);
    let project = projects
        .create(
            &owner,
            ProjectCreate {
                slug: None,
                display_name: "Fixture".into(),
                settings: ProjectSettings {
                    brand_name: "Fixture".into(),
                    market: "generic".into(),
                    language: "en".into(),
                    initial_sources: vec![InitialSource {
                        kind: InitialSourceKind::Url,
                        value: "https://example.invalid/source".into(),
                        visibility: InitialSourceVisibility::Public,
                        version_ref: None,
                        content_hash: None,
                    }],
                    ..ProjectSettings::default()
                },
            },
        )
        .await
        .unwrap();
    let settings = settings_hash(&project.settings.clone().validate_start().unwrap()).unwrap();
    let started = projects
        .start(
            &owner,
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("binding-fixture"),
                request_hash: start_request_hash(project.id, project.revision, &settings),
                settings_hash: settings,
                operation_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    (
        TenantScope::new(operator.into(), tenant.into(), Some(project.id)),
        started.cycle_id,
    )
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL in GEO_TEST_DATABASE_URL"]
async fn encrypted_binding_is_pre_send_scoped_and_database_immutable() {
    let url =
        std::env::var("GEO_TEST_DATABASE_URL").expect("disposable test database URL required");
    let admin = sqlx::PgPool::connect(&url).await.unwrap();
    let schema = format!("binding_test_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let options = url
        .parse::<sqlx::postgres::PgConnectOptions>()
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_with(options)
        .await
        .unwrap();
    let database = Database::from_pool(pool);
    database.migrate().await.unwrap();

    let (scope, cycle) = scope_with_cycle(&database).await;
    let (foreign, _) = scope_with_cycle(&database).await;
    let repo = PgChannelJobRepository::from_database(&database);
    let now = Utc::now();
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let measurement = Uuid::new_v4();
    let publish = |target_id| ChannelTarget {
        target_id,
        input: ChannelTargetInput::Publish {
            source_id: Uuid::new_v4(),
            source_version_id: Uuid::new_v4(),
            platform: "fixture".into(),
            account_id: Uuid::new_v4(),
            title: "title".into(),
            body: "body".into(),
            body_sha256: "fixture".into(),
        },
    };
    repo.create_plan(
        &scope,
        ChannelPlan {
            plan_id: Uuid::new_v4(),
            project_id: scope.project_id.unwrap(),
            cycle_id: cycle,
            input_hash: "binding".into(),
            revision: 1,
            created_at: now,
            targets: vec![
                publish(first),
                publish(second),
                ChannelTarget {
                    target_id: measurement,
                    input: ChannelTargetInput::Measure {
                        account_id: Uuid::new_v4(),
                        provider: "fixture".into(),
                        model: "fixture".into(),
                        surface: "web".into(),
                        search_mode: "off".into(),
                        protocol_version: "v1".into(),
                        question_set_version: "v1".into(),
                        question: "question".into(),
                        market: "generic".into(),
                        language: "en".into(),
                        scheduled_at: now,
                        sample_ordinal: 0,
                    },
                },
            ],
        },
    )
    .await
    .unwrap();
    let attempt = Uuid::new_v4();
    let measured = Uuid::new_v4();
    repo.claim(&scope, first, attempt, now).await.unwrap();
    repo.claim(&scope, measurement, measured, now)
        .await
        .unwrap();
    assert!(
        repo.get_publication_binding(&scope, first, attempt)
            .await
            .unwrap()
            .is_none()
    );
    let binding = ChannelSecret::new(vec![1, 2, 3, 4]);
    repo.store_publication_binding(&scope, first, attempt, binding.clone())
        .await
        .unwrap();
    repo.store_publication_binding(&scope, first, attempt, binding.clone())
        .await
        .unwrap();
    assert_eq!(
        repo.store_publication_binding(&scope, first, attempt, ChannelSecret::new(vec![8]))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    for (scoped, target) in [(&foreign, first), (&scope, second)] {
        assert_eq!(
            repo.get_publication_binding(scoped, target, attempt)
                .await
                .err()
                .unwrap()
                .code,
            ErrorCode::NotFound
        );
        assert_eq!(
            repo.store_publication_binding(scoped, target, attempt, binding.clone())
                .await
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
    }
    assert_eq!(
        repo.store_publication_binding(&scope, measurement, measured, binding.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );
    let outcome = ChannelOutcome {
        status: ChannelOutcomeStatus::Unknown,
        detail: None,
        occurred_at: now,
        raw_answer: None,
        citations: vec![],
        public_url: None,
        screenshot_ref: None,
        connector_version: None,
        runner_evidence: vec![],
        fixture: true,
    };
    repo.finish(&scope, first, attempt, outcome.clone(), now)
        .await
        .unwrap();
    // A distinct repository instance models recovery after a process restart.
    let recovered = PgChannelJobRepository::from_database(&database);
    assert_eq!(
        recovered
            .get_publication_binding(&scope, first, attempt)
            .await
            .unwrap()
            .unwrap()
            .encrypted_bytes(),
        binding.encrypted_bytes()
    );
    recovered
        .store_publication_binding(&scope, first, attempt, binding.clone())
        .await
        .unwrap();
    let second_attempt = Uuid::new_v4();
    repo.claim(&scope, second, second_attempt, now)
        .await
        .unwrap();
    repo.finish(&scope, second, second_attempt, outcome, now)
        .await
        .unwrap();
    assert_eq!(
        repo.store_publication_binding(&scope, second, second_attempt, binding.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let update = sqlx::query(
        "UPDATE publication_execution_bindings SET encrypted_binding=$1 WHERE attempt_id=$2",
    )
    .bind(vec![9u8])
    .bind(attempt)
    .execute(database.pool())
    .await;
    assert!(update.is_err(), "database must reject binding replacement");
    let delete = sqlx::query("DELETE FROM publication_execution_bindings WHERE attempt_id=$1")
        .bind(attempt)
        .execute(database.pool())
        .await;
    assert!(delete.is_err(), "database must reject binding deletion");
    let direct_after_finish = sqlx::query(
        "INSERT INTO publication_execution_bindings \
         (attempt_id,operator_id,tenant_id,project_id,target_id,encrypted_binding) \
         VALUES($1,$2,$3,$4,$5,$6)",
    )
    .bind(second_attempt)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(scope.project_id.unwrap().as_uuid())
    .bind(first)
    .bind(vec![1u8])
    .execute(database.pool())
    .await;
    assert!(
        direct_after_finish.is_err(),
        "database must reject post-send insert"
    );
    let direct_measure = sqlx::query(
        "INSERT INTO publication_execution_bindings \
         (attempt_id,operator_id,tenant_id,project_id,target_id,encrypted_binding) \
         VALUES($1,$2,$3,$4,$5,$6)",
    )
    .bind(measured)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(scope.project_id.unwrap().as_uuid())
    .bind(measurement)
    .bind(vec![1u8])
    .execute(database.pool())
    .await;
    assert!(
        direct_measure.is_err(),
        "database must reject measurement binding"
    );
    let wrong_target = sqlx::query(
        "INSERT INTO publication_execution_bindings \
         (attempt_id,operator_id,tenant_id,project_id,target_id,encrypted_binding) \
         VALUES($1,$2,$3,$4,$5,$6)",
    )
    .bind(attempt)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(scope.project_id.unwrap().as_uuid())
    .bind(second)
    .bind(vec![1u8])
    .execute(database.pool())
    .await;
    assert!(
        wrong_target.is_err(),
        "database must reject a binding on a different target"
    );
}
