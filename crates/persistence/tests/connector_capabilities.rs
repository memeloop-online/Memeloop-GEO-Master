//! Operator-scoped connector registry against an isolated disposable schema.
use chrono::Utc;
use geo_domain::{
    ChannelOutcome, ChannelOutcomeStatus, ConnectorAvailability, ConnectorCapabilityRepository,
    ConnectorKey, ConnectorVerification, ErrorCode,
};
use geo_persistence::{Database, PgConnectorCapabilityRepository};
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
    database.pool().close().await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
}
