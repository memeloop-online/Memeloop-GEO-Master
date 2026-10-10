use geo_domain::{
    DistributionScope, DistributionScopeMode, InitialSource, InitialSourceKind,
    InitialSourceVisibility, ProjectCreate, ProjectPatch, ProjectRepository, ProjectSettings,
    ProjectStartCommand, TenantScope, hash_idempotency_key, settings_hash, start_request_hash,
};
use geo_persistence::{Database, DatabaseConfig, PgProjectRepository};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires disposable GEO_TEST_DATABASE_URL"]
async fn successor_is_atomic_scoped_frozen_and_recoverable() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database URL");
    let database = Database::connect_and_migrate(&DatabaseConfig::from_url(url.clone()).unwrap())
        .await
        .expect("migrations");
    let pool = database.pool();
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    let other_tenant = Uuid::new_v4();
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'Test')")
        .bind(operator)
        .bind(format!("cycle-{operator}"))
        .execute(pool)
        .await
        .unwrap();
    for tenant_id in [tenant, other_tenant] {
        sqlx::query(
            "INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,'Test')",
        )
        .bind(tenant_id)
        .bind(operator)
        .bind(format!("cycle-{tenant_id}"))
        .execute(pool)
        .await
        .unwrap();
    }
    let scope = TenantScope::new(operator.into(), tenant.into(), None);
    let repo = PgProjectRepository::from_database(&database);
    let project = repo
        .create(
            &scope,
            ProjectCreate {
                slug: None,
                display_name: "Weekly cycle".to_owned(),
                settings: ProjectSettings {
                    brand_name: "Example".to_owned(),
                    market: "US".to_owned(),
                    language: "en".to_owned(),
                    initial_sources: vec![InitialSource {
                        kind: InitialSourceKind::Text,
                        value: "Public example".to_owned(),
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
    let accepted = repo
        .start(
            &scope,
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("successor-start"),
                request_hash: start_request_hash(project.id, project.revision, &hash),
                settings_hash: hash,
                operation_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    let original = repo.get_start(&scope, project.id).await.unwrap().unwrap();
    let original_snapshot: (Uuid, String, String, String) = sqlx::query_as(
        "SELECT cycle.config_revision_id, config.settings::text,
                document.input_refs::text, distribution.input_refs::text
         FROM optimization_cycles cycle
         JOIN project_config_revisions config ON config.config_revision_id=cycle.config_revision_id
         JOIN document_manifests document ON document.cycle_id=cycle.cycle_id
         JOIN distribution_manifests distribution ON distribution.cycle_id=cycle.cycle_id
         WHERE cycle.cycle_id=$1",
    )
    .bind(accepted.cycle_id)
    .fetch_one(pool)
    .await
    .unwrap();
    let first_scope = DistributionScope {
        mode: DistributionScopeMode::Explicit,
        included_platform_ids: vec!["channel-one".to_owned()],
        ..Default::default()
    };
    let earlier = original.cutoff_at - chrono::Duration::seconds(1);
    assert!(
        repo.schedule_next_cycle(&scope, project.id, accepted.cycle_id, earlier)
            .await
            .is_err()
    );
    let revision = repo
        .get(&scope, project.id)
        .await
        .unwrap()
        .unwrap()
        .revision;
    repo.update(
        &scope,
        project.id,
        revision,
        ProjectPatch {
            report_timezone: Some("America/New_York".to_owned()),
            distribution_scope: Some(first_scope.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let frozen = project.settings.clone().validate_start().unwrap();
    assert_eq!(
        repo.get_cycle_settings(&scope, project.id, accepted.cycle_id)
            .await
            .unwrap(),
        Some(frozen.clone())
    );
    let foreign_scope = TenantScope::new(operator.into(), other_tenant.into(), None);
    assert!(
        repo.get_cycle_settings(&foreign_scope, project.id, accepted.cycle_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repo.schedule_next_cycle(
            &foreign_scope,
            project.id,
            accepted.cycle_id,
            original.cutoff_at
        )
        .await
        .is_err()
    );
    let pending_before = repo
        .list_pending_successor_cycles_after(100, None)
        .await
        .unwrap();
    assert!(
        !pending_before
            .iter()
            .any(|candidate| candidate.predecessor_cycle_id == accepted.cycle_id)
    );
    // A revision-1 report persisted by the reducer is the scanner's trigger.
    sqlx::query(
        "INSERT INTO report_snapshots
         (report_id,operator_id,tenant_id,project_id,cycle_id,revision,
          report_window_start_at,report_window_end_at,cutoff_at,reducer_version,
          input_manifest_versions,input_hash,snapshot)
         VALUES ($1,$2,$3,$4,$5,1,$6,$7,$8,'test',$9,'test',$10)",
    )
    .bind(Uuid::new_v4())
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(accepted.cycle_id)
    .bind(original.report_window_start_at)
    .bind(original.report_window_end_at)
    .bind(original.cutoff_at)
    .bind(serde_json::json!({}))
    .bind(serde_json::json!({}))
    .execute(pool)
    .await
    .unwrap();
    let pending = repo
        .list_pending_successor_cycles_after(100, None)
        .await
        .unwrap();
    assert!(pending.iter().any(|candidate| {
        candidate.predecessor_cycle_id == accepted.cycle_id
            && candidate.scope.project_id == Some(project.id)
    }));
    let now = original.cutoff_at + chrono::Duration::days(120);
    let (left, right) = tokio::join!(
        repo.schedule_next_cycle(&scope, project.id, accepted.cycle_id, now),
        repo.schedule_next_cycle(&scope, project.id, accepted.cycle_id, now)
    );
    let successor = left.unwrap();
    assert_eq!(successor, right.unwrap());
    assert_eq!(
        successor,
        repo.schedule_next_cycle(
            &scope,
            project.id,
            accepted.cycle_id,
            original.cutoff_at + chrono::Duration::seconds(1),
        )
        .await
        .unwrap()
    );
    assert_eq!(successor.report_timezone, original.report_timezone);
    assert_eq!(
        successor.report_window_start_at,
        original.report_window_end_at
    );
    assert!(successor.report_window_end_at > successor.report_window_start_at);
    assert_eq!(
        successor.cutoff_at,
        original.cutoff_at + chrono::Duration::days(7)
    );
    assert!(successor.cutoff_at < now);
    assert!(!successor.document_manifest.as_ref().unwrap().sealed);
    assert!(!successor.distribution_manifest.as_ref().unwrap().sealed);
    let mut expected_settings = frozen.clone();
    expected_settings.distribution_scope = first_scope.clone();
    assert_eq!(
        repo.get_cycle_settings(&scope, project.id, successor.cycle_id)
            .await
            .unwrap(),
        Some(expected_settings.clone())
    );
    let successor_config: (Uuid, i64, String, String, Option<serde_json::Value>) = sqlx::query_as(
        "SELECT config.config_revision_id, config.project_revision, config.settings_hash,
                    config.settings::text, config.estimate_snapshot
             FROM project_config_revisions config
             JOIN optimization_cycles cycle ON cycle.config_revision_id=config.config_revision_id
             WHERE cycle.cycle_id=$1",
    )
    .bind(successor.cycle_id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_ne!(successor_config.0, original_snapshot.0);
    assert_eq!(successor_config.1, revision + 1);
    assert_eq!(
        successor_config.2,
        settings_hash(&expected_settings).unwrap()
    );
    assert_eq!(
        serde_json::from_str::<ProjectSettings>(&successor_config.3).unwrap(),
        expected_settings
    );
    assert_eq!(
        successor_config.4, None,
        "old estimate cannot describe new targets"
    );
    let successor_manifest: (String, serde_json::Value) = sqlx::query_as(
        "SELECT scope_hash, input_refs FROM distribution_manifests WHERE cycle_id=$1",
    )
    .bind(successor.cycle_id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(successor_manifest.0, successor_config.2);
    assert_eq!(
        successor_manifest.1["distribution_scope"],
        serde_json::to_value(&first_scope).unwrap()
    );
    let original_after: (Uuid, String, String, String) = sqlx::query_as(
        "SELECT cycle.config_revision_id, config.settings::text,
                document.input_refs::text, distribution.input_refs::text
         FROM optimization_cycles cycle
         JOIN project_config_revisions config ON config.config_revision_id=cycle.config_revision_id
         JOIN document_manifests document ON document.cycle_id=cycle.cycle_id
         JOIN distribution_manifests distribution ON distribution.cycle_id=cycle.cycle_id
         WHERE cycle.cycle_id=$1",
    )
    .bind(accepted.cycle_id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(original_after, original_snapshot);
    let next_scope = DistributionScope {
        mode: DistributionScopeMode::Explicit,
        included_platform_ids: vec!["channel-two".to_owned()],
        ..Default::default()
    };
    let latest_revision = repo
        .get(&scope, project.id)
        .await
        .unwrap()
        .unwrap()
        .revision;
    repo.update(
        &scope,
        project.id,
        latest_revision,
        ProjectPatch {
            distribution_scope: Some(next_scope.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    drop(repo);
    drop(database);
    let database = Database::connect_and_migrate(&DatabaseConfig::from_url(url).unwrap())
        .await
        .expect("reconnect");
    let repo = PgProjectRepository::from_database(&database);
    assert_eq!(
        repo.schedule_next_cycle(&scope, project.id, accepted.cycle_id, now)
            .await
            .unwrap(),
        successor
    );
    let third = repo
        .schedule_next_cycle(&scope, project.id, successor.cycle_id, now)
        .await
        .unwrap();
    let mut third_settings = expected_settings.clone();
    third_settings.distribution_scope = next_scope;
    assert_eq!(
        repo.get_cycle_settings(&scope, project.id, third.cycle_id)
            .await
            .unwrap(),
        Some(third_settings)
    );
    assert_eq!(
        repo.get_cycle_settings(&scope, project.id, successor.cycle_id)
            .await
            .unwrap(),
        Some(expected_settings)
    );
    assert_eq!(
        repo.get_current_cycle(&scope, project.id).await.unwrap(),
        Some(third.clone())
    );
    assert_eq!(
        repo.get_start(&scope, project.id).await.unwrap(),
        Some(original)
    );
    assert_eq!(
        repo.get_report_cycle(&scope, project.id, successor.cycle_id)
            .await
            .unwrap(),
        Some(successor)
    );
    assert!(
        repo.list_pending_successor_cycles_after(100, None)
            .await
            .unwrap()
            .iter()
            .all(|candidate| candidate.predecessor_cycle_id != accepted.cycle_id)
    );
}
