//! Requires an explicitly disposable PostgreSQL database.

use chrono::{Duration, Utc};
use geo_domain::{
    ChannelJobRepository, ChannelPlan, ChannelTarget, ChannelTargetInput, InitialSource,
    InitialSourceKind, InitialSourceVisibility, ProjectCreate, ProjectRepository, ProjectSettings,
    ProjectStartCommand, TenantScope, hash_idempotency_key, settings_hash, start_request_hash,
};
use geo_persistence::{Database, PgChannelJobRepository, PgProjectRepository};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn pending_scan_keyset_scope_due_and_project_state() {
    // The dispatcher intentionally scans all tenants. Its expected global
    // result set must not include fixtures left by other repository suites.
    let url =
        std::env::var("GEO_TEST_DATABASE_URL").expect("disposable test database URL required");
    let admin = sqlx::PgPool::connect(&url).await.unwrap();
    let schema = format!("dispatcher_test_{}", Uuid::new_v4().simple());
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
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,$3)")
        .bind(operator)
        .bind(format!("dispatcher-{operator}"))
        .bind("Dispatcher test")
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,$4)",
    )
    .bind(tenant)
    .bind(operator)
    .bind(format!("dispatcher-{tenant}"))
    .bind("Tenant")
    .execute(database.pool())
    .await
    .unwrap();
    let projects = PgProjectRepository::from_database(&database);
    let repo = PgChannelJobRepository::from_database(&database);
    let tenant_scope = TenantScope::new(operator.into(), tenant.into(), None);
    let now = Utc::now();
    let mut expected = Vec::new();
    let mut held_future = None;
    let mut other_scope = None;
    let mut active_account = None;
    for (ordinal, status) in ["active", "paused", "archived"].into_iter().enumerate() {
        let project = projects
            .create(
                &tenant_scope,
                ProjectCreate {
                    slug: None,
                    display_name: format!("Fixture {ordinal}"),
                    settings: ProjectSettings {
                        brand_name: "Fixture".into(),
                        market: "US".into(),
                        language: "en".into(),
                        initial_sources: vec![InitialSource {
                            kind: InitialSourceKind::Url,
                            value: "https://example.com".into(),
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
        let hash = settings_hash(&project.settings.clone().validate_start().unwrap()).unwrap();
        let started = projects
            .start(
                &tenant_scope,
                project.id,
                ProjectStartCommand {
                    expected_revision: project.revision,
                    idempotency_key_hash: hash_idempotency_key(&format!("dispatcher-{ordinal}")),
                    request_hash: start_request_hash(project.id, project.revision, &hash),
                    settings_hash: hash,
                    operation_id: Uuid::new_v4(),
                },
            )
            .await
            .unwrap();
        let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.id));
        let due_ids = [Uuid::new_v4(), Uuid::new_v4()];
        let future_id = Uuid::new_v4();
        let account_id = Uuid::new_v4();
        let target = |target_id, scheduled_at| ChannelTarget {
            target_id,
            input: ChannelTargetInput::Measure {
                account_id,
                provider: "fixture".into(),
                model: "fixture".into(),
                surface: "consumer_web".into(),
                search_mode: "web_search".into(),
                protocol_version: "v1".into(),
                question_set_version: "v1".into(),
                question: "Fixture?".into(),
                market: "US".into(),
                language: "en".into(),
                scheduled_at,
                sample_ordinal: 0,
            },
        };
        repo.create_plan(
            &scope,
            ChannelPlan {
                plan_id: Uuid::new_v4(),
                project_id: project.id,
                cycle_id: started.cycle_id,
                input_hash: format!("dispatcher-{ordinal}"),
                revision: 1,
                created_at: now,
                targets: vec![
                    target(due_ids[0], now - Duration::seconds(1)),
                    target(due_ids[1], now - Duration::seconds(1)),
                    target(future_id, now + Duration::days(1)),
                ],
            },
        )
        .await
        .unwrap();
        if status == "active" {
            active_account = Some(account_id);
            expected.extend(due_ids.into_iter().map(|id| (id, scope.clone())));
            held_future = Some(future_id);
        } else {
            if status == "paused" {
                other_scope = Some(scope.clone());
            }
            sqlx::query("UPDATE projects SET status=$1 WHERE project_id=$2")
                .bind(status)
                .bind(project.id.as_uuid())
                .execute(database.pool())
                .await
                .unwrap();
        }
    }
    expected.sort_by_key(|(id, _)| *id);
    let first = repo.scan_pending(None, now, 1).await.unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].target_id, expected[0].0);
    assert_eq!(first[0].scope, expected[0].1);
    let second = repo
        .scan_pending(Some(first[0].target_id), now, 1)
        .await
        .unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].target_id, expected[1].0);
    assert!(
        repo.scan_pending(Some(second[0].target_id), now, 1)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        !repo
            .scan_pending(None, now, 100)
            .await
            .unwrap()
            .iter()
            .any(|c| c.target_id == held_future.unwrap())
    );
    repo.claim(&expected[0].1, expected[0].0, Uuid::new_v4(), now)
        .await
        .unwrap();
    let remaining = repo.scan_pending(None, now, 100).await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].target_id, expected[1].0);
    let account = active_account.unwrap();
    let owner = Uuid::new_v4();
    repo.reserve_account(
        &expected[0].1,
        account,
        owner,
        now,
        now + Duration::seconds(2),
    )
    .await
    .unwrap();
    let another_project = other_scope.unwrap();
    assert!(
        repo.reserve_account(
            &another_project,
            account,
            Uuid::new_v4(),
            now,
            now + Duration::minutes(5),
        )
        .await
        .is_err()
    );
    let later = now + Duration::seconds(3);
    let new_owner = Uuid::new_v4();
    repo.reserve_account(
        &another_project,
        account,
        new_owner,
        later,
        later + Duration::minutes(5),
    )
    .await
    .unwrap();
    repo.release_account(&expected[0].1, account, owner)
        .await
        .unwrap();
    assert!(
        repo.claim_reserved(&expected[1].1, expected[1].0, Uuid::new_v4(), owner, later,)
            .await
            .is_err()
    );
    assert!(
        repo.get_target(&expected[1].1, expected[1].0)
            .await
            .unwrap()
            .attempts
            .is_empty()
    );
    assert!(
        repo.reserve_account(
            &expected[0].1,
            account,
            Uuid::new_v4(),
            later,
            later + Duration::minutes(5),
        )
        .await
        .is_err()
    );
    database.pool().close().await;
    // Only this test's freshly generated schema is removed.
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
}
