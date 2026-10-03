use geo_domain::{
    InitialSource, InitialSourceKind, InitialSourceVisibility, ProjectCreate, ProjectPatch,
    ProjectRepository, ProjectSettings, ProjectStartCommand, TenantScope, hash_idempotency_key,
    settings_hash, start_request_hash,
};
use geo_persistence::{Database, DatabaseConfig, PgProjectRepository};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires disposable GEO_TEST_DATABASE_URL"]
async fn successor_is_atomic_scoped_frozen_and_recoverable() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database URL");
    let database = Database::connect_and_migrate(&DatabaseConfig::from_url(url).unwrap())
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
    assert_eq!(
        repo.get_cycle_settings(&scope, project.id, successor.cycle_id)
            .await
            .unwrap(),
        Some(frozen),
        "new cycle carries its predecessor's frozen configuration"
    );
    assert_eq!(
        repo.get_current_cycle(&scope, project.id).await.unwrap(),
        Some(successor.clone())
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
