//! Operator-scoped connector registry against an isolated disposable schema.
use chrono::Utc;
use geo_domain::{
    ChannelJobRepository, ChannelOutcome, ChannelOutcomeStatus, ChannelPlan, ChannelTarget,
    ChannelTargetInput, ConnectorAvailability, ConnectorCapabilityRepository, ConnectorKey,
    ConnectorVerification, ErrorCode, PLAIN_TEXT_ARTICLE_FORMAT, TenantScope,
    plain_text_article_readback_hash,
};
use geo_persistence::{Database, PgChannelJobRepository, PgConnectorCapabilityRepository};
use sha2::{Digest, Sha256};
use uuid::Uuid;

fn verification() -> ConnectorVerification {
    let now = Utc::now();
    let url = "https://example.com/posts/123".to_string();
    let hash = "c".repeat(64);
    let publication_receipt = ChannelOutcome {
        status: ChannelOutcomeStatus::Published,
        detail: None,
        occurred_at: now,
        raw_answer: None,
        citations: vec![],
        public_url: Some(url.clone()),
        screenshot_ref: None,
        connector_version: Some("trusted.v1".into()),
        runner_evidence: vec![],
        fixture: false,
    };
    let public_readback = ChannelOutcome {
        status: ChannelOutcomeStatus::Verified,
        runner_evidence: vec![serde_json::json!({
            "kind":"public_readback","url":url,"content_matched":true,
            "owned_by_account":true,"expected_sha256":hash,"readback_sha256":hash
        })],
        ..publication_receipt.clone()
    };
    ConnectorVerification {
        verification_id: Uuid::new_v4(),
        key: ConnectorKey {
            platform_id: "creator".into(),
            placement_slot: "primary".into(),
        },
        connector_version: "trusted.v1".into(),
        content_type: "article".into(),
        publication_receipt,
        public_readback,
        verified_at: now,
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL in GEO_TEST_DATABASE_URL"]
async fn operator_registry_is_durable_revisioned_and_immutable() {
    let url =
        std::env::var("GEO_TEST_DATABASE_URL").expect("disposable test database URL required");
    let admin = sqlx::PgPool::connect(&url).await.unwrap();
    let schema = format!("connector_test_{}", Uuid::new_v4().simple());
    // Test-only schema name consists solely of this fixed prefix and generated UUID hex.
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
    let other = Uuid::new_v4();
    for id in [operator, other] {
        sqlx::query("INSERT INTO operators(operator_id,slug,display_name) VALUES($1,$2,'fixture')")
            .bind(id)
            .bind(format!("connector-test-{id}"))
            .execute(database.pool())
            .await
            .unwrap();
    }
    let repo = PgConnectorCapabilityRepository::from_database(&database);
    let record = verification();
    let key = record.key.clone();
    let operator = operator.into();
    let other = other.into();
    assert_eq!(
        repo.resolve(operator, &key, "trusted.v1", "article")
            .await
            .unwrap()
            .availability,
        ConnectorAvailability::Unavailable
    );
    assert_eq!(
        repo.configure(
            operator,
            key.clone(),
            0,
            true,
            vec!["article".into()],
            "trusted.v1"
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::InvalidRequest
    );
    let mut fixture = record.clone();
    fixture.verification_id = Uuid::new_v4();
    fixture.publication_receipt.fixture = true;
    assert_eq!(
        repo.insert_verification(operator, fixture)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );
    repo.insert_verification(operator, record.clone())
        .await
        .unwrap();
    assert_eq!(
        repo.insert_verification(operator, record.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert!(
        repo.configure(
            other,
            key.clone(),
            0,
            true,
            vec!["article".into()],
            "trusted.v1"
        )
        .await
        .is_err()
    );
    assert!(
        repo.configure(
            operator,
            key.clone(),
            0,
            true,
            vec!["image".into()],
            "trusted.v1"
        )
        .await
        .is_err()
    );
    repo.configure(
        operator,
        key.clone(),
        0,
        true,
        vec!["article".into()],
        "trusted.v1",
    )
    .await
    .unwrap();
    assert_eq!(
        repo.resolve(operator, &key, "trusted.v1", "article")
            .await
            .unwrap()
            .availability,
        ConnectorAvailability::Available
    );
    assert_eq!(
        repo.resolve(operator, &key, "trusted.v2", "article")
            .await
            .unwrap()
            .availability,
        ConnectorAvailability::VersionMismatch
    );
    assert_eq!(
        repo.resolve(operator, &key, "trusted.v1", "image")
            .await
            .unwrap()
            .availability,
        ConnectorAvailability::UnsupportedContentType
    );
    assert_eq!(
        repo.resolve(other, &key, "trusted.v1", "article")
            .await
            .unwrap()
            .availability,
        ConnectorAvailability::Unavailable
    );
    assert_eq!(
        repo.configure(operator, key.clone(), 0, false, vec![], "trusted.v1")
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        repo.configure(operator, key.clone(), 1, false, vec![], "trusted.v2")
            .await
            .unwrap()
            .revision,
        2
    );
    assert_eq!(
        repo.resolve(operator, &key, "trusted.v1", "article")
            .await
            .unwrap()
            .availability,
        ConnectorAvailability::Disabled
    );
    assert_eq!(repo.history(operator, &key).await.unwrap().len(), 1);
    let mutation =
        sqlx::query("DELETE FROM connector_capability_verifications WHERE verification_id=$1")
            .bind(record.verification_id)
            .execute(database.pool())
            .await;
    assert!(
        mutation.is_err(),
        "historical verification cannot be deleted"
    );
    let mutation = sqlx::query(
        "UPDATE connector_capability_verifications SET connector_version='trusted.v2' WHERE verification_id=$1",
    )
    .bind(record.verification_id)
    .execute(database.pool())
    .await;
    assert!(
        mutation.is_err(),
        "historical verification cannot be rewritten"
    );
    // A saved source attempt is the only path that bootstraps a real format
    // without operator self-attestation; the scan does not send anything.
    let tenant = Uuid::new_v4();
    let project = Uuid::new_v4();
    let revision = Uuid::new_v4();
    let cycle = Uuid::new_v4();
    let source = Uuid::new_v4();
    let version = Uuid::new_v4();
    let account = Uuid::new_v4();
    let target_id = Uuid::new_v4();
    let attempt_id = Uuid::new_v4();
    // Both event order and idempotent replay must survive PostgreSQL's
    // microsecond timestamp rounding.
    let now = chrono::DateTime::<Utc>::from_timestamp(Utc::now().timestamp(), 123_456_789).unwrap();
    sqlx::query(
        "INSERT INTO tenants(tenant_id,operator_id,slug,display_name) VALUES($1,$2,$3,'test')",
    )
    .bind(tenant)
    .bind(operator.as_uuid())
    .bind(format!("test-{tenant}"))
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO projects(project_id,operator_id,tenant_id,slug,display_name) VALUES($1,$2,$3,$4,'test')")
        .bind(project).bind(operator.as_uuid()).bind(tenant).bind(format!("test-{project}"))
        .execute(database.pool()).await.unwrap();
    sqlx::query("INSERT INTO project_config_revisions(config_revision_id,operator_id,tenant_id,project_id,project_revision,settings,source_refs,settings_hash) VALUES($1,$2,$3,$4,1,'{}','[]','test')")
        .bind(revision).bind(operator.as_uuid()).bind(tenant).bind(project)
        .execute(database.pool()).await.unwrap();
    sqlx::query("INSERT INTO optimization_cycles(cycle_id,operator_id,tenant_id,project_id,config_revision_id,state,report_timezone,report_window_start_at,report_window_end_at,cutoff_at) VALUES($1,$2,$3,$4,$5,'running','UTC',$6,$7,$7)")
        .bind(cycle).bind(operator.as_uuid()).bind(tenant).bind(project).bind(revision)
        .bind(now).bind(now + chrono::Duration::days(7))
        .execute(database.pool()).await.unwrap();
    sqlx::query("INSERT INTO knowledge_sources(source_id,operator_id,tenant_id,project_id,revision,kind,name,purpose,state,locator) VALUES($1,$2,$3,$4,1,'text','test','public','active','{}')")
        .bind(source).bind(operator.as_uuid()).bind(tenant).bind(project)
        .execute(database.pool()).await.unwrap();
    sqlx::query("INSERT INTO knowledge_source_versions(source_version_id,operator_id,tenant_id,project_id,source_id,version,content_sha256,captured_at,parser_version,extraction_version) VALUES($1,$2,$3,$4,$5,1,$6,$7,'v1','v1')")
        .bind(version).bind(operator.as_uuid()).bind(tenant).bind(project).bind(source)
        .bind("a".repeat(64)).bind(now)
        .execute(database.pool()).await.unwrap();
    sqlx::query("INSERT INTO operator_channel_accounts(operator_id,account_id,platform,metadata) VALUES($1,$2,'sourcepub','{}')")
        .bind(operator.as_uuid()).bind(account)
        .execute(database.pool()).await.unwrap();
    sqlx::query("INSERT INTO operator_channel_assignments(operator_id,tenant_id,project_id,account_id) VALUES($1,$2,$3,$4)")
        .bind(operator.as_uuid()).bind(tenant).bind(project).bind(account)
        .execute(database.pool()).await.unwrap();
    let scope = TenantScope::new(operator, tenant.into(), Some(project.into()));
    let jobs = PgChannelJobRepository::from_database(&database);
    jobs.create_plan(
        &scope,
        ChannelPlan {
            plan_id: Uuid::new_v4(),
            project_id: project.into(),
            cycle_id: cycle,
            input_hash: "source-test".into(),
            revision: 1,
            created_at: now,
            targets: vec![ChannelTarget {
                target_id,
                input: ChannelTargetInput::Publish {
                    source_id: source,
                    source_version_id: version,
                    platform: "sourcepub".into(),
                    account_id: account,
                    title: "Title".into(),
                    body: "Body".into(),
                    body_sha256: hex::encode(Sha256::digest(b"Body")),
                },
            }],
        },
    )
    .await
    .unwrap();
    jobs.claim(&scope, target_id, attempt_id, now)
        .await
        .unwrap();
    // Revocation after claim cannot retroactively invalidate the saved proof.
    sqlx::query("DELETE FROM operator_channel_assignments WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND account_id=$4")
        .bind(operator.as_uuid()).bind(tenant).bind(project).bind(account)
        .execute(database.pool()).await.unwrap();
    let readback_hash = plain_text_article_readback_hash("Title", "Body");
    let saved_outcome = ChannelOutcome {
        status: ChannelOutcomeStatus::Verified,
        detail: None,
        occurred_at: now,
        raw_answer: None,
        citations: vec![],
        public_url: Some("https://example.com/p/123".into()),
        screenshot_ref: None,
        connector_version: Some("source_derived.unverified.v1".into()),
        fixture: false,
        runner_evidence: vec![
            serde_json::json!({"kind":"public_readback","url":"https://example.com/p/123",
                "content_matched":true,"owned_by_account":true,
                "expected_sha256":readback_hash,"readback_sha256":readback_hash}),
            serde_json::json!({"kind":"runner_receipt","schema_version":"geo.runner.receipt.v1",
                "provenance":"live","execution_id":attempt_id,
                "connector_version":"source_derived.unverified.v1","occurred_at":now}),
        ],
    };
    jobs.finish(&scope, target_id, attempt_id, saved_outcome, now)
        .await
        .unwrap();
    assert!(
        repo.project_saved_publication_verification(
            &TenantScope::new(other, tenant.into(), Some(project.into())),
            target_id,
            attempt_id
        )
        .await
        .is_err()
    );
    let candidates = repo.scan_unprojected_publications(None, 10).await.unwrap();
    assert_eq!(candidates, vec![(scope.clone(), target_id, attempt_id)]);
    let saved = repo
        .project_saved_publication_verification(&scope, target_id, attempt_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.content_type, PLAIN_TEXT_ARTICLE_FORMAT);
    assert_eq!(
        repo.project_saved_publication_verification(&scope, target_id, attempt_id)
            .await
            .unwrap()
            .unwrap(),
        saved
    );
    assert!(
        repo.scan_unprojected_publications(None, 10)
            .await
            .unwrap()
            .is_empty()
    );
    let settings = repo.get(operator, &saved.key).await.unwrap().unwrap();
    assert_eq!(settings.content_types, vec![PLAIN_TEXT_ARTICLE_FORMAT]);
    assert!(settings.enabled);
    assert_eq!(
        repo.resolve(
            operator,
            &saved.key,
            "source_derived.unverified.v1",
            PLAIN_TEXT_ARTICLE_FORMAT
        )
        .await
        .unwrap()
        .availability,
        ConnectorAvailability::Available
    );
    let disabled = repo
        .configure(
            operator,
            saved.key.clone(),
            1,
            false,
            vec![],
            "source_derived.unverified.v1",
        )
        .await
        .unwrap();
    assert!(!disabled.enabled);
    repo.project_saved_publication_verification(&scope, target_id, attempt_id)
        .await
        .unwrap();
    assert_eq!(
        repo.get(operator, &saved.key).await.unwrap().unwrap(),
        disabled
    );
    database.pool().close().await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
}
