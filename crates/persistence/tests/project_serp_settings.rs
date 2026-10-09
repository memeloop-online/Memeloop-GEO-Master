use chrono::{Duration, Utc};
use geo_domain::*;
use geo_persistence::{
    Database, DatabaseConfig, MemorySerpRepository, PgProjectSerpSettingsRepository,
    PgSerpRepository,
};
use std::sync::Arc;
use uuid::Uuid;

fn write(source_key: &str, encrypted_credentials: Option<Vec<u8>>) -> ProjectSerpSettingsWrite {
    ProjectSerpSettingsWrite {
        source_key: source_key.into(),
        provider: ProjectSerpProvider::Dataforseo,
        enabled: encrypted_credentials.is_some(),
        protocol_defaults: SerpProtocol {
            query: String::new(),
            engine: SerpEngine::Google,
            surface: SerpSurface::ThirdPartyApi,
            source: "dataforseo".into(),
            source_location_code: "2840".into(),
            country: "US".into(),
            city: None,
            language: "en".into(),
            device: SerpDevice::Desktop,
            operating_system: "windows".into(),
            requested_depth: 10,
            max_pages: 1,
            priority: 1,
            login: "unspecified".into(),
            personalization: "unspecified".into(),
            protocol_version: SERP_PROTOCOL_VERSION.into(),
            connector_version: "synthetic.v1".into(),
        },
        encrypted_credentials,
    }
}

async fn contract(
    store: &dyn ProjectSerpSettingsRepository,
    scope: &TenantScope,
    other: &TenantScope,
) {
    assert!(store.get(scope, "primary").await.unwrap().is_none());
    assert!(store.list(scope, None, 100).await.unwrap().is_empty());
    let mut missing = write("primary", None);
    missing.enabled = true;
    assert!(store.save(scope, 0, missing).await.is_err());
    assert!(store.get(scope, "primary").await.unwrap().is_none());
    let empty = store.save(scope, 0, write("primary", None)).await.unwrap();
    assert_eq!(empty.revision, 1);
    assert_eq!(empty.active_credential_revision, None);
    // Ciphertext is opaque to the repository; AEAD authentication belongs to
    // SecretEnvelope at the service boundary. These are synthetic byte payloads.
    let first_cipher = vec![1; 64];
    let second_cipher = vec![2; 64];
    let first = store
        .save(scope, 1, write("primary", Some(first_cipher.clone())))
        .await
        .unwrap();
    assert_eq!(first.active_credential_revision, Some(2));
    let first_secret = store
        .get_credential(scope, "primary", 2)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_secret.encrypted_credentials, first_cipher);
    assert!(
        store
            .get_credential(scope, "primary", 1)
            .await
            .unwrap()
            .is_none()
    );
    let (left, right) = tokio::join!(
        store.save(scope, 2, write("primary", Some(second_cipher.clone()))),
        store.save(scope, 2, write("primary", Some(second_cipher.clone())))
    );
    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    let error = if let Err(error) = left {
        error
    } else {
        right.unwrap_err()
    };
    assert_eq!(error.code, ErrorCode::Conflict);
    let rotated = store.get(scope, "primary").await.unwrap().unwrap();
    assert_eq!(rotated.revision, 3);
    assert_eq!(rotated.active_credential_revision, Some(3));
    assert_eq!(
        store
            .get_credential(scope, "primary", 2)
            .await
            .unwrap()
            .unwrap(),
        first_secret
    );
    assert_eq!(
        store
            .get_credential(scope, "primary", 3)
            .await
            .unwrap()
            .unwrap()
            .encrypted_credentials,
        second_cipher
    );
    let disabled = store.save(scope, 3, write("primary", None)).await.unwrap();
    assert!(!disabled.enabled);
    assert_eq!(disabled.active_credential_revision, Some(3));
    assert!(
        store
            .get_credential(scope, "primary", 2)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .get_credential(scope, "primary", 3)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .save(scope, 3, write("primary", Some(vec![3; 64])))
            .await
            .is_err()
    );
    assert!(
        store
            .get_credential(scope, "primary", 4)
            .await
            .unwrap()
            .is_none()
    );
    for foreign in [
        other.clone(),
        TenantScope::new(Uuid::new_v4().into(), scope.tenant_id, scope.project_id),
        TenantScope::new(scope.operator_id, Uuid::new_v4().into(), scope.project_id),
    ] {
        assert!(store.get(&foreign, "primary").await.unwrap().is_none());
        assert!(
            store
                .get_credential(&foreign, "primary", 2)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .list(&foreign, Some("primary".into()), 1)
                .await
                .is_err()
        );
    }
    let secondary = store
        .save(scope, 0, write("secondary", Some(vec![4; 64])))
        .await
        .unwrap();
    assert_eq!(secondary.active_credential_revision, Some(1));
    assert_eq!(secondary.protocol_defaults, disabled.protocol_defaults);
    let first_page = store.list(scope, None, 1).await.unwrap();
    assert_eq!(first_page, vec![disabled.clone()]);
    assert_eq!(
        store
            .list(scope, Some(first_page[0].source_key.clone()), 1)
            .await
            .unwrap(),
        vec![secondary]
    );
    assert!(store.list(scope, None, 101).await.is_err());
    let mut cursor = None;
    let mut found = Vec::new();
    loop {
        let page = store.list_dispatch_sources(cursor, 1).await.unwrap();
        let Some(row) = page.first() else {
            break;
        };
        cursor = Some(row.cursor().unwrap());
        if row.scope == *scope {
            found.push(row.source_key.clone());
        }
    }
    assert_eq!(
        found,
        vec!["primary", "secondary"],
        "disabled sources retain recovery inventory"
    );
    let mut invalid = write("third", None);
    invalid.protocol_defaults.query = "not a default".into();
    assert!(store.save(scope, 0, invalid).await.is_err());
    let no_project = TenantScope::new(scope.operator_id, scope.tenant_id, None);
    assert!(store.get(&no_project, "primary").await.is_err());
    assert!(store.get_credential(scope, "primary", 0).await.is_err());
    let aad = project_serp_credential_aad(scope, "primary", 2).unwrap();
    assert_ne!(
        aad,
        project_serp_credential_aad(scope, "primary", 3).unwrap()
    );
    assert_ne!(
        aad,
        project_serp_credential_aad(scope, "secondary", 2).unwrap()
    );
    assert_ne!(
        aad,
        project_serp_credential_aad(other, "primary", 2).unwrap()
    );
    assert_eq!(
        format!("{first_secret:?}"),
        "ProjectSerpCredentialRecord([redacted])"
    );
}

async fn send_guard_contract(
    settings: &dyn ProjectSerpSettingsRepository,
    serp: &dyn SerpRepository,
    scope: &TenantScope,
) {
    let initial = write("guard", Some(vec![1; 64]));
    settings.save(scope, 0, initial.clone()).await.unwrap();
    let now = Utc::now();
    let mut protocol = initial.protocol_defaults;
    protocol.query = "synthetic queued question".into();
    let measurement = SerpMeasurement {
        measurement_id: Uuid::new_v4(),
        source_key: "guard".into(),
        protocol,
        target: None,
        target_rule_version: SERP_TARGET_RULE_VERSION.into(),
        question_binding: None,
        scheduled_at: now,
        created_at: now,
        state: SerpTaskState::Queued,
    };
    serp.accept(scope, "guard-first", measurement.clone())
        .await
        .unwrap();
    let claim = serp
        .claim(
            scope,
            measurement.measurement_id,
            now,
            now + Duration::minutes(5),
        )
        .await
        .unwrap()
        .unwrap();
    let request_sha = sha256_hex(b"synthetic-request");
    settings.save(scope, 1, write("guard", None)).await.unwrap();
    assert!(
        serp.begin_send(scope, &claim, &request_sha, "guard-tag", Some(1), now)
            .await
            .is_err()
    );
    assert!(
        serp.get_execution(scope, measurement.measurement_id)
            .await
            .unwrap()
            .unwrap()
            .intent
            .is_none()
    );
    let mut enabled = write("guard", None);
    enabled.enabled = true;
    settings.save(scope, 2, enabled.clone()).await.unwrap();
    enabled.protocol_defaults.language = "fr".into();
    settings.save(scope, 3, enabled).await.unwrap();
    let intent = serp
        .begin_send(scope, &claim, &request_sha, "guard-tag", Some(1), now)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(intent.credential_revision, Some(1));
    assert_eq!(
        serp.get(scope, measurement.measurement_id)
            .await
            .unwrap()
            .unwrap()
            .protocol
            .language,
        "en"
    );
    settings
        .save(scope, 4, write("guard", Some(vec![5; 64])))
        .await
        .unwrap();
    assert!(
        serp.begin_send(scope, &claim, &request_sha, "guard-tag", Some(1), now)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        serp.get_execution(scope, measurement.measurement_id)
            .await
            .unwrap()
            .unwrap()
            .intent,
        Some(intent)
    );
    assert_eq!(
        settings
            .get_credential(scope, "guard", 1)
            .await
            .unwrap()
            .unwrap()
            .encrypted_credentials,
        vec![1; 64]
    );

    let mut second = measurement.clone();
    second.measurement_id = Uuid::new_v4();
    serp.accept(scope, "guard-second", second.clone())
        .await
        .unwrap();
    let claim = serp
        .claim(
            scope,
            second.measurement_id,
            now,
            now + Duration::minutes(5),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        serp.begin_send(
            scope,
            &claim,
            &request_sha,
            "guard-second-tag",
            Some(1),
            now
        )
        .await
        .is_err()
    );
    let (send, disable) = tokio::join!(
        serp.begin_send(
            scope,
            &claim,
            &request_sha,
            "guard-second-tag",
            Some(5),
            now
        ),
        settings.save(scope, 5, write("guard", None))
    );
    assert!(!disable.unwrap().enabled);
    let execution = serp
        .get_execution(scope, second.measurement_id)
        .await
        .unwrap()
        .unwrap();
    match send {
        Ok(Some(intent)) => assert_eq!(execution.intent, Some(intent)),
        Err(_) => assert!(execution.intent.is_none()),
        Ok(None) => panic!("first submission cannot be an idempotent replay"),
    }
    let mut missing = measurement;
    missing.measurement_id = Uuid::new_v4();
    missing.source_key = "unconfigured".into();
    serp.accept(scope, "guard-missing", missing.clone())
        .await
        .unwrap();
    let claim = serp
        .claim(
            scope,
            missing.measurement_id,
            now,
            now + Duration::minutes(5),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        serp.begin_send(scope, &claim, &request_sha, "guard-missing", Some(1), now)
            .await
            .is_err()
    );
}

async fn postgres_project_pause_guard(database: &Database, scope: &TenantScope) {
    let serp = PgSerpRepository::from_database(database);
    let now = Utc::now();
    let mut protocol = write("project-status", None).protocol_defaults;
    protocol.query = "synthetic project status question".into();
    let measurement = SerpMeasurement {
        measurement_id: Uuid::new_v4(),
        source_key: "project-status".into(),
        protocol,
        target: None,
        target_rule_version: SERP_TARGET_RULE_VERSION.into(),
        question_binding: None,
        scheduled_at: now,
        created_at: now,
        state: SerpTaskState::Queued,
    };
    serp.accept(scope, "project-status", measurement.clone())
        .await
        .unwrap();
    let claim = serp
        .claim(
            scope,
            measurement.measurement_id,
            now,
            now + Duration::minutes(5),
        )
        .await
        .unwrap()
        .unwrap();
    let request_sha = sha256_hex(b"synthetic-project-status-request");
    let mut tx = database.pool().begin().await.unwrap();
    sqlx::query("UPDATE projects SET status='paused' WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3")
        .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(scope.project_id.unwrap().as_uuid())
        .execute(&mut *tx).await.unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let sender = serp.clone();
    let send_scope = scope.clone();
    let send_claim = claim.clone();
    let send_sha = request_sha.clone();
    let mut pending = tokio::spawn(async move {
        let _ = started_tx.send(());
        sender
            .begin_send(
                &send_scope,
                &send_claim,
                &send_sha,
                "project-status-tag",
                None,
                now,
            )
            .await
    });
    started_rx.await.unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut pending)
            .await
            .is_err(),
        "first send must wait for the project update transaction"
    );
    tx.commit().await.unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), pending)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(
        serp.get_execution(scope, measurement.measurement_id)
            .await
            .unwrap()
            .unwrap()
            .intent
            .is_none()
    );
    sqlx::query("UPDATE projects SET status='archived' WHERE project_id=$1")
        .bind(scope.project_id.unwrap().as_uuid())
        .execute(database.pool())
        .await
        .unwrap();
    assert!(
        serp.begin_send(scope, &claim, &request_sha, "project-status-tag", None, now)
            .await
            .is_err()
    );
    sqlx::query("UPDATE projects SET status='draft' WHERE project_id=$1")
        .bind(scope.project_id.unwrap().as_uuid())
        .execute(database.pool())
        .await
        .unwrap();
    let intent = serp
        .begin_send(scope, &claim, &request_sha, "project-status-tag", None, now)
        .await
        .unwrap()
        .unwrap();
    sqlx::query("UPDATE projects SET status='paused' WHERE project_id=$1")
        .bind(scope.project_id.unwrap().as_uuid())
        .execute(database.pool())
        .await
        .unwrap();
    assert_eq!(
        serp.get_execution(scope, measurement.measurement_id)
            .await
            .unwrap()
            .unwrap()
            .intent,
        Some(intent)
    );
    assert!(
        serp.begin_send(scope, &claim, &request_sha, "project-status-tag", None, now)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn memory_settings_rotate_preserve_and_scope_credentials() {
    let scope = TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    );
    let other = TenantScope::new(
        scope.operator_id,
        scope.tenant_id,
        Some(Uuid::new_v4().into()),
    );
    let settings = Arc::new(MemoryProjectSerpSettingsRepository::default());
    contract(settings.as_ref(), &scope, &other).await;
    send_guard_contract(
        settings.as_ref(),
        &MemorySerpRepository::with_settings(settings.clone()),
        &scope,
    )
    .await;
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn postgres_settings_rotate_preserve_and_scope_credentials() {
    let config = DatabaseConfig::from_url(
        std::env::var("GEO_TEST_DATABASE_URL").expect("disposable PostgreSQL URL required"),
    )
    .unwrap();
    let database = Database::connect_and_migrate(&config).await.unwrap();
    let (operator, tenant, project, other_project) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    sqlx::query("INSERT INTO operators(operator_id,slug,display_name) VALUES($1,$2,'Synthetic')")
        .bind(operator)
        .bind(format!("serp-settings-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants(tenant_id,operator_id,slug,display_name) VALUES($1,$2,$3,'Synthetic')",
    )
    .bind(tenant)
    .bind(operator)
    .bind(format!("serp-settings-{tenant}"))
    .execute(database.pool())
    .await
    .unwrap();
    for id in [project, other_project] {
        sqlx::query("INSERT INTO projects(project_id,operator_id,tenant_id,slug,display_name) VALUES($1,$2,$3,$4,'Synthetic')")
            .bind(id).bind(operator).bind(tenant).bind(format!("serp-settings-{id}")).execute(database.pool()).await.unwrap();
    }
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.into()));
    let other = TenantScope::new(operator.into(), tenant.into(), Some(other_project.into()));
    contract(
        &PgProjectSerpSettingsRepository::from_database(&database),
        &scope,
        &other,
    )
    .await;
    send_guard_contract(
        &PgProjectSerpSettingsRepository::from_database(&database),
        &PgSerpRepository::from_database(&database),
        &scope,
    )
    .await;
    postgres_project_pause_guard(&database, &scope).await;
    let restarted = PgProjectSerpSettingsRepository::from_database(&database);
    assert_eq!(
        restarted
            .get(&scope, "primary")
            .await
            .unwrap()
            .unwrap()
            .active_credential_revision,
        Some(3)
    );
    assert_eq!(
        restarted
            .get_credential(&scope, "primary", 2)
            .await
            .unwrap()
            .unwrap()
            .encrypted_credentials,
        vec![1; 64]
    );
    assert_eq!(
        restarted
            .get_credential(&scope, "primary", 3)
            .await
            .unwrap()
            .unwrap()
            .encrypted_credentials,
        vec![2; 64]
    );
}
