//! Disposable PostgreSQL integration tests for channel accounts and execution.
//! These tests write fixture rows and must never target a shared database.

use chrono::{Duration, Utc};
use geo_domain::{
    ChannelAccount, ChannelAccountRecord, ChannelCycleInputs, ChannelGroup, ChannelJobRepository,
    ChannelOutcome, ChannelOutcomeStatus, ChannelOwnerKind, ChannelPlan, ChannelRepository,
    ChannelSecret, ChannelSettings, ChannelSettingsRecord, ChannelStatus, ChannelTarget,
    ChannelTargetInput, ErrorCode, InitialSource, InitialSourceKind, InitialSourceVisibility,
    PoolAccount, PoolAccountRecord, PoolGroup, ProjectCreate, ProjectRepository, ProjectSettings,
    ProjectStartCommand, ReportManifestKind, ReportMeasurementStatus, ReportPublicationStatus,
    TenantScope, hash_idempotency_key, settings_hash, start_request_hash,
};
use geo_persistence::{
    Database, DatabaseConfig, PgChannelJobRepository, PgChannelRepository, PgProjectRepository,
};
use uuid::Uuid;

struct Fixture {
    database: Database,
    scope: TenantScope,
    other_project: TenantScope,
    other_tenant: TenantScope,
    cycle_id: Uuid,
}

async fn fixture() -> Fixture {
    let config = DatabaseConfig::from_url(
        std::env::var("GEO_TEST_DATABASE_URL").expect("disposable test database URL required"),
    )
    .expect("valid database URL");
    let database = Database::connect_and_migrate(&config)
        .await
        .expect("migrations");
    let operator = Uuid::new_v4();
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,$3)")
        .bind(operator)
        .bind(format!("channel-{operator}"))
        .bind("Fixture operator")
        .execute(database.pool())
        .await
        .unwrap();
    let tenants = [Uuid::new_v4(), Uuid::new_v4()];
    for tenant in tenants {
        sqlx::query(
            "INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,$4)",
        )
        .bind(tenant)
        .bind(operator)
        .bind(format!("channel-{tenant}"))
        .bind("Fixture tenant")
        .execute(database.pool())
        .await
        .unwrap();
    }
    let projects = PgProjectRepository::from_database(&database);
    let mut scopes = Vec::new();
    let mut cycle_id = None;
    for tenant in [tenants[0], tenants[0], tenants[1]] {
        let tenant_scope = TenantScope::new(operator.into(), tenant.into(), None);
        let created = projects
            .create(
                &tenant_scope,
                ProjectCreate {
                    slug: None,
                    display_name: "Channel fixture".to_owned(),
                    settings: ProjectSettings {
                        brand_name: "Fixture".to_owned(),
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
            .unwrap();
        if cycle_id.is_none() {
            let hash = settings_hash(&created.settings.clone().validate_start().unwrap()).unwrap();
            let started = projects
                .start(
                    &tenant_scope,
                    created.id,
                    ProjectStartCommand {
                        expected_revision: created.revision,
                        idempotency_key_hash: hash_idempotency_key("channel-fixture"),
                        request_hash: start_request_hash(created.id, created.revision, &hash),
                        settings_hash: hash,
                        operation_id: Uuid::new_v4(),
                    },
                )
                .await
                .unwrap();
            cycle_id = Some(started.cycle_id);
        }
        scopes.push(TenantScope::new(
            tenant_scope.operator_id,
            tenant_scope.tenant_id,
            Some(created.id),
        ));
    }
    Fixture {
        database,
        scope: scopes[0].clone(),
        other_project: scopes[1].clone(),
        other_tenant: scopes[2].clone(),
        cycle_id: cycle_id.unwrap(),
    }
}

fn envelope() -> ChannelSecret {
    // Opaque synthetic envelope: encryption happens in the trusted API layer.
    ChannelSecret::new(vec![0x67, 0x65, 0x6f, 0x01, 0x80, 0xff, 0x7f])
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn channel_accounts_secrets_and_operator_assignments_are_scoped_in_postgres() {
    let f = fixture().await;
    let repo = PgChannelRepository::from_database(&f.database);
    let now = Utc::now();
    let group = ChannelGroup {
        group_id: Uuid::new_v4(),
        project_id: f.scope.project_id.unwrap(),
        name: "Fixture group".into(),
        created_at: now,
    };
    repo.save_group(&f.scope, group.clone()).await.unwrap();
    assert_eq!(repo.list_groups(&f.scope).await.unwrap().len(), 1);
    assert!(repo.list_groups(&f.other_project).await.unwrap().is_empty());
    assert!(repo.list_groups(&f.other_tenant).await.unwrap().is_empty());
    assert_eq!(
        repo.save_group(&f.other_project, group.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Forbidden
    );
    repo.save_settings(
        &f.scope,
        ChannelSettingsRecord {
            settings: ChannelSettings {
                project_id: f.scope.project_id.unwrap(),
                default_group_id: Some(group.group_id),
                proxy_configured: true,
                proxy_server: None,
                updated_at: now,
            },
            proxy: Some(envelope()),
        },
    )
    .await
    .unwrap();
    assert!(repo.get_settings(&f.other_project).await.unwrap().is_none());
    assert_eq!(
        repo.get_settings(&f.scope)
            .await
            .unwrap()
            .unwrap()
            .proxy
            .unwrap()
            .encrypted_bytes(),
        envelope().encrypted_bytes()
    );

    let account_id = Uuid::new_v4();
    repo.save_account(
        &f.scope,
        ChannelAccountRecord {
            account: ChannelAccount {
                account_id,
                project_id: f.scope.project_id.unwrap(),
                owner_kind: ChannelOwnerKind::Customer,
                platform: "fixture_platform".into(),
                group_id: Some(group.group_id),
                status: ChannelStatus::Ready,
                display_name: Some("Fixture account".into()),
                platform_account_id: Some(format!("fixture-{account_id}")),
                avatar_url: None,
                enabled: true,
                proxy_configured: true,
                proxy_server: None,
                created_at: now,
                updated_at: now,
            },
            session: Some(envelope()),
            proxy: Some(envelope()),
        },
    )
    .await
    .unwrap();
    let reopened = PgChannelRepository::from_database(&f.database)
        .get_account(&f.scope, account_id)
        .await
        .unwrap();
    assert_eq!(reopened.account.group_id, Some(group.group_id));
    assert_eq!(
        reopened.session.unwrap().encrypted_bytes(),
        envelope().encrypted_bytes()
    );
    assert_eq!(
        reopened.proxy.unwrap().encrypted_bytes(),
        envelope().encrypted_bytes()
    );
    let (metadata, session, proxy): (serde_json::Value, Vec<u8>, Vec<u8>) = sqlx::query_as(
        "SELECT metadata,encrypted_session,encrypted_proxy FROM channel_accounts \
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND account_id=$4",
    )
    .bind(f.scope.operator_id.as_uuid())
    .bind(f.scope.tenant_id.as_uuid())
    .bind(f.scope.project_id.unwrap().as_uuid())
    .bind(account_id)
    .fetch_one(f.database.pool())
    .await
    .unwrap();
    assert_eq!(session, envelope().encrypted_bytes());
    assert_eq!(proxy, envelope().encrypted_bytes());
    for forbidden in ["session", "proxy", "encrypted_session", "encrypted_proxy"] {
        assert!(metadata.get(forbidden).is_none());
    }
    for scope in [&f.other_project, &f.other_tenant] {
        assert!(repo.list_accounts(scope).await.unwrap().is_empty());
        assert_eq!(
            repo.get_account(scope, account_id)
                .await
                .err()
                .unwrap()
                .code,
            ErrorCode::NotFound
        );
    }
    let pool_group = PoolGroup {
        group_id: Uuid::new_v4(),
        name: "Fixture pool".into(),
        created_at: now,
    };
    repo.save_pool_group(f.scope.operator_id, pool_group.clone())
        .await
        .unwrap();
    let pool_id = Uuid::new_v4();
    repo.save_pool_account(
        f.scope.operator_id,
        PoolAccountRecord {
            account: PoolAccount {
                account_id: pool_id,
                platform: "fixture_platform".into(),
                group_id: Some(pool_group.group_id),
                status: ChannelStatus::Ready,
                display_name: Some("Pooled fixture".into()),
                platform_account_id: None,
                avatar_url: None,
                enabled: true,
                proxy_configured: true,
                proxy_server: None,
                created_at: now,
                updated_at: now,
            },
            session: Some(envelope()),
            proxy: Some(envelope()),
        },
    )
    .await
    .unwrap();
    let stored_pool = repo
        .get_pool_account(f.scope.operator_id, pool_id)
        .await
        .unwrap();
    assert_eq!(
        stored_pool.session.unwrap().encrypted_bytes(),
        envelope().encrypted_bytes()
    );
    assert_eq!(
        stored_pool.proxy.unwrap().encrypted_bytes(),
        envelope().encrypted_bytes()
    );
    for scope in [&f.scope, &f.other_project, &f.other_tenant] {
        assert!(
            repo.list_assigned_pool_accounts(scope)
                .await
                .unwrap()
                .is_empty()
        );
    }
    repo.assign_pool_account(&f.scope, pool_id, true)
        .await
        .unwrap();
    repo.assign_pool_account(&f.other_tenant, pool_id, true)
        .await
        .unwrap();
    assert_eq!(
        repo.list_assigned_pool_accounts(&f.scope)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        repo.list_assigned_pool_accounts(&f.other_tenant)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        repo.list_assigned_pool_accounts(&f.other_project)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        repo.list_pool_assignments(f.scope.operator_id, pool_id)
            .await
            .unwrap()
            .len(),
        2
    );
    repo.assign_pool_account(&f.scope, pool_id, false)
        .await
        .unwrap();
    assert!(
        repo.list_assigned_pool_accounts(&f.scope)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        repo.list_assigned_pool_accounts(&f.other_tenant)
            .await
            .unwrap()
            .len(),
        1
    );
}

fn assert_denominator(inputs: &ChannelCycleInputs) {
    assert_eq!(inputs.manifests.len(), 2);
    assert_eq!(inputs.manifests[0].kind, ReportManifestKind::Distribution);
    assert_eq!(inputs.manifests[0].expected_count, Some(1));
    assert!(inputs.manifests[0].sealed);
    assert_eq!(inputs.manifests[1].kind, ReportManifestKind::Measurement);
    assert_eq!(inputs.manifests[1].expected_count, Some(2));
    assert!(inputs.manifests[1].sealed);
    assert_eq!(inputs.publications.as_ref().unwrap().len(), 1);
    assert_eq!(inputs.measurements.as_ref().unwrap().len(), 2);
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn frozen_channel_plan_claim_finish_and_report_denominator_in_postgres() {
    let f = fixture().await;
    let repo = PgChannelJobRepository::from_database(&f.database);
    let now = chrono::DateTime::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap();
    let publication_id = Uuid::new_v4();
    let measurement_ids = [Uuid::new_v4(), Uuid::new_v4()];
    let account_id = Uuid::new_v4();
    let measure = |ordinal| ChannelTargetInput::Measure {
        account_id,
        provider: "fixture_provider".into(),
        model: "fixture_model".into(),
        surface: "consumer_web".into(),
        search_mode: "search".into(),
        protocol_version: "v1".into(),
        question_set_version: "v1".into(),
        question: "Generic fixture question".into(),
        market: "global".into(),
        language: "en".into(),
        scheduled_at: now,
        sample_ordinal: ordinal,
    };
    let plan = ChannelPlan {
        plan_id: Uuid::new_v4(),
        project_id: f.scope.project_id.unwrap(),
        cycle_id: f.cycle_id,
        input_hash: "fixture-frozen-input".into(),
        revision: 1,
        created_at: now,
        targets: vec![
            ChannelTarget {
                target_id: publication_id,
                input: ChannelTargetInput::Publish {
                    source_id: Uuid::new_v4(),
                    source_version_id: Uuid::new_v4(),
                    platform: "fixture_platform".into(),
                    account_id,
                    title: "Fixture title".into(),
                    body: "Fixture body".into(),
                    body_sha256: "fixture-hash".into(),
                },
            },
            ChannelTarget {
                target_id: measurement_ids[0],
                input: measure(0),
            },
            ChannelTarget {
                target_id: measurement_ids[1],
                input: measure(1),
            },
        ],
    };
    assert_eq!(
        repo.create_plan(&f.scope, plan.clone()).await.unwrap(),
        plan
    );
    let mut replay = plan.clone();
    replay.plan_id = Uuid::new_v4();
    replay.targets.clear();
    assert_eq!(repo.create_plan(&f.scope, replay).await.unwrap(), plan);
    let mut conflict = plan.clone();
    conflict.input_hash = "changed-input".into();
    assert_eq!(
        repo.create_plan(&f.scope, conflict).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    assert_eq!(
        repo.get_plan(&f.scope, f.cycle_id).await.unwrap(),
        Some(plan)
    );
    for scope in [&f.other_project, &f.other_tenant] {
        assert!(repo.get_plan(scope, f.cycle_id).await.unwrap().is_none());
        assert_eq!(
            repo.get_target(scope, publication_id)
                .await
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
        assert!(
            repo.cycle_inputs(scope, f.cycle_id, now)
                .await
                .unwrap()
                .manifests
                .is_empty()
        );
    }
    let before = repo.cycle_inputs(&f.scope, f.cycle_id, now).await.unwrap();
    assert_denominator(&before);
    assert_eq!(
        before.publications.unwrap()[0].status,
        ReportPublicationStatus::Planned
    );
    let attempt_id = Uuid::new_v4();
    let claimed_at = now + Duration::seconds(1);
    let (target, attempt) = repo
        .claim(&f.scope, publication_id, attempt_id, claimed_at)
        .await
        .unwrap();
    assert_eq!(target.target_id, publication_id);
    assert_eq!(attempt.attempt_id, attempt_id);
    assert!(attempt.outcome.is_none());
    assert_eq!(
        repo.claim(&f.scope, publication_id, Uuid::new_v4(), claimed_at)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let crashed = repo
        .cycle_inputs(&f.scope, f.cycle_id, claimed_at)
        .await
        .unwrap();
    assert_denominator(&crashed);
    assert_eq!(
        crashed.publications.unwrap()[0].status,
        ReportPublicationStatus::Unknown
    );
    let outcome = ChannelOutcome {
        status: ChannelOutcomeStatus::Verified,
        detail: Some("Fixture readback".into()),
        occurred_at: claimed_at,
        raw_answer: None,
        citations: vec![],
        public_url: Some("https://example.com/fixture".into()),
        screenshot_ref: None,
        connector_version: Some("fixture-v1".into()),
        runner_evidence: vec![],
        fixture: true,
    };
    let finished_at = claimed_at + Duration::seconds(1);
    let finished = repo
        .finish(
            &f.scope,
            publication_id,
            attempt_id,
            outcome.clone(),
            finished_at,
        )
        .await
        .unwrap();
    assert_eq!(finished.attempts.len(), 1);
    assert_eq!(finished.attempts[0].outcome, Some(outcome.clone()));
    assert_eq!(
        repo.finish(
            &f.scope,
            publication_id,
            attempt_id,
            outcome.clone(),
            finished_at,
        )
        .await
        .unwrap(),
        finished
    );
    let mut changed = outcome;
    changed.status = ChannelOutcomeStatus::Failed;
    assert_eq!(
        repo.finish(&f.scope, publication_id, attempt_id, changed, finished_at)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let before_receipt = repo
        .cycle_inputs(&f.scope, f.cycle_id, claimed_at)
        .await
        .unwrap();
    assert_denominator(&before_receipt);
    assert_eq!(
        before_receipt.publications.unwrap()[0].status,
        ReportPublicationStatus::Unknown
    );
    let after = repo
        .cycle_inputs(&f.scope, f.cycle_id, finished_at)
        .await
        .unwrap();
    assert_denominator(&after);
    assert_eq!(
        after.publications.unwrap()[0].status,
        ReportPublicationStatus::Verified
    );
    assert_eq!(
        after.measurements.as_ref().unwrap()[0].status,
        ReportMeasurementStatus::Pending
    );
    assert_eq!(
        after.measurements.as_ref().unwrap()[1].status,
        ReportMeasurementStatus::Pending
    );
    let measure_attempt = Uuid::new_v4();
    repo.claim(&f.scope, measurement_ids[0], measure_attempt, finished_at)
        .await
        .unwrap();
    let measured = repo
        .cycle_inputs(&f.scope, f.cycle_id, finished_at)
        .await
        .unwrap();
    assert_denominator(&measured);
    assert_eq!(
        measured.measurements.as_ref().unwrap()[0].status,
        ReportMeasurementStatus::Missing
    );
    assert_eq!(
        measured.measurements.as_ref().unwrap()[1].status,
        ReportMeasurementStatus::Pending
    );
}
