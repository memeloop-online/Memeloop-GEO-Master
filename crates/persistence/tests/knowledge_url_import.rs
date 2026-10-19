use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use geo_domain::{
    AppError, ChunkLocator, ErrorCode, FetchedUrlPage, ImportItem, ImportStatus, KnowledgePurpose,
    KnowledgeRepository, ProjectCreate, ProjectRepository, ProjectSettings, SourceKind,
    SourceTextBasis, TenantScope, UrlPageFetcher, sha256_hex,
};
use geo_persistence::{Database, DatabaseConfig, PgKnowledgeRepository, PgProjectRepository};
use sqlx::Row;
use tokio::sync::Barrier;
use uuid::Uuid;

#[derive(Debug)]
struct SnapshotFetcher {
    calls: AtomicUsize,
    barrier: Option<Barrier>,
    fail: bool,
}

#[async_trait]
impl UrlPageFetcher for SnapshotFetcher {
    async fn fetch(&self, url: &str) -> Result<FetchedUrlPage, AppError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(AppError::new(
                ErrorCode::DependencyUnavailable,
                "URL acquisition failed",
            ));
        }
        if let Some(barrier) = &self.barrier {
            tokio::time::timeout(std::time::Duration::from_secs(5), barrier.wait())
                .await
                .expect("concurrent acquisition must not hold the receipt lock");
        }
        Ok(FetchedUrlPage {
            requested_url: url.to_owned(),
            final_url: "https://example.org/final".into(),
            captured_at: Utc.timestamp_opt(1_800_000_000, 0).unwrap(),
            raw_bytes: b"<html><body><h1>Snapshot</h1><p>Archived page text.</p></body></html>"
                .to_vec(),
            media_type: "text/html".into(),
            text: "Snapshot\n\nArchived page text.".into(),
            parser_version: "html-test-v1".into(),
        })
    }
}

async fn database() -> Database {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable PostgreSQL database");
    Database::connect_and_migrate(&DatabaseConfig::from_url(url).unwrap())
        .await
        .unwrap()
}

async fn scope(database: &Database) -> TenantScope {
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    sqlx::query("INSERT INTO operators(operator_id,slug,display_name) VALUES($1,$2,'Synthetic')")
        .bind(operator)
        .bind(format!("url-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants(tenant_id,operator_id,slug,display_name) VALUES($1,$2,$3,'Synthetic')",
    )
    .bind(tenant)
    .bind(operator)
    .bind(format!("url-{tenant}"))
    .execute(database.pool())
    .await
    .unwrap();
    let project = PgProjectRepository::from_database(database)
        .create(
            &TenantScope::new(operator.into(), tenant.into(), None),
            ProjectCreate {
                slug: Some(format!("url-{tenant}")),
                display_name: "Synthetic URL import".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    TenantScope::new(operator.into(), tenant.into(), Some(project.id))
}

fn item() -> ImportItem {
    ImportItem {
        client_item_id: "synthetic-page".into(),
        kind: SourceKind::Url,
        name: "Synthetic page".into(),
        purpose: KnowledgePurpose::Internal,
        text: None,
        url: Some("https://example.org/requested".into()),
        object_id: None,
        knowledge_release_id: None,
    }
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn url_snapshot_receipt_provenance_and_scoped_content_survive_repository_restart() {
    let database = database().await;
    let scope = scope(&database).await;
    let other = self::scope(&database).await;
    let fetcher = Arc::new(SnapshotFetcher {
        calls: AtomicUsize::new(0),
        barrier: None,
        fail: false,
    });
    let repository =
        PgKnowledgeRepository::from_database(&database).with_url_fetcher(fetcher.clone());
    assert!(repository.capabilities(&scope).await.unwrap().url_fetch);
    let imported = repository
        .import_batch(&scope, vec![item()])
        .await
        .unwrap()
        .items
        .remove(0);
    assert_eq!(imported.status, ImportStatus::Succeeded);
    let source = imported.source.as_ref().unwrap();
    let version = imported.source_version.as_ref().unwrap();
    assert_eq!(source.kind, SourceKind::Url);
    assert_eq!(source.locator["url"], "https://example.org/requested");
    assert_eq!(source.locator["final_url"], "https://example.org/final");
    assert_eq!(
        version.original_url.as_deref(),
        Some("https://example.org/requested")
    );
    assert_eq!(
        version.captured_at,
        Utc.timestamp_opt(1_800_000_000, 0).unwrap()
    );
    assert_eq!(version.parser_version, "html-test-v1");
    let object = sqlx::query(
        "SELECT object.sha256,object.object_version,object.detected_media_type,
                snapshot.content,snapshot.object_version AS snapshot_version
         FROM knowledge_stored_objects object JOIN knowledge_url_snapshot_bytes snapshot
           ON snapshot.object_id=object.object_id WHERE object.object_id=$1",
    )
    .bind(version.object_id)
    .fetch_one(database.pool())
    .await
    .unwrap();
    let bytes: Vec<u8> = object.get("content");
    assert_eq!(version.content_sha256, sha256_hex(&bytes));
    assert_eq!(object.get::<String, _>("sha256"), version.content_sha256);
    assert_eq!(object.get::<String, _>("detected_media_type"), "text/html");
    assert_eq!(
        Some(object.get::<i64, _>("object_version")),
        version.object_version
    );
    assert_eq!(
        Some(object.get::<i64, _>("snapshot_version")),
        version.object_version
    );
    assert!(bytes.starts_with(b"<html>"));
    let detail = repository
        .get_source_detail(&scope, source.source_id)
        .await
        .unwrap()
        .unwrap();
    assert!(!detail.chunks.is_empty());
    for chunk in detail.chunks {
        assert!(matches!(chunk.locator, ChunkLocator::Web {
            snapshot_object_id, original_url, selector: None, start_char: None, end_char: None,
        } if Some(snapshot_object_id) == version.object_id && original_url == "https://example.org/requested"));
    }
    let content = repository
        .get_source_version_content(&scope, source.source_id, version.source_version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(content.text_basis, SourceTextBasis::Extracted);
    assert_eq!(content.text, "Snapshot\n\nArchived page text.");
    let restarted = PgKnowledgeRepository::from_database(&database);
    assert_eq!(
        restarted
            .import_batch(&scope, vec![item()])
            .await
            .unwrap()
            .items[0],
        imported
    );
    let mut changed = item();
    changed.url = Some("https://example.org/changed".into());
    let conflict = repository
        .import_batch(&scope, vec![changed])
        .await
        .unwrap()
        .items
        .remove(0);
    assert_eq!(conflict.error.unwrap().code, ErrorCode::Conflict);
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);
    assert!(
        repository
            .get_source_detail(&other, source.source_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repository
            .get_source_version_content(&other, source.source_id, version.source_version_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repository
            .get_attachment_object(&scope, version.object_id.unwrap())
            .await
            .unwrap()
            .is_none()
    );
    let uploads: i64 = sqlx::query_scalar("SELECT count(*) FROM knowledge_upload_sessions WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3")
        .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(scope.project_id.unwrap().as_uuid())
        .fetch_one(database.pool()).await.unwrap();
    assert_eq!(uploads, 0, "server snapshots do not fabricate user uploads");
    let mut corrupt = bytes;
    corrupt[0] = b'!';
    sqlx::query("UPDATE knowledge_url_snapshot_bytes SET content=$1 WHERE object_id=$2")
        .bind(corrupt)
        .bind(version.object_id)
        .execute(database.pool())
        .await
        .unwrap();
    assert_eq!(
        repository
            .get_source_version_content(&scope, source.source_id, version.source_version_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn concurrent_url_acquisition_rechecks_receipt_before_creating_sources() {
    let database = database().await;
    let scope = scope(&database).await;
    let fetcher = Arc::new(SnapshotFetcher {
        calls: AtomicUsize::new(0),
        barrier: Some(Barrier::new(2)),
        fail: false,
    });
    let repository =
        PgKnowledgeRepository::from_database(&database).with_url_fetcher(fetcher.clone());
    let (left, right) = tokio::join!(
        repository.import_batch(&scope, vec![item()]),
        repository.import_batch(&scope, vec![item()]),
    );
    let left = left.unwrap();
    assert_eq!(left, right.unwrap());
    assert_eq!(left.items[0].status, ImportStatus::Succeeded);
    assert_eq!(repository.list_sources(&scope).await.unwrap().len(), 1);
    assert_eq!(
        repository.current_release(&scope).await.unwrap().sequence,
        Some(1)
    );
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn url_acquisition_failure_has_a_replayable_receipt_without_a_source_or_release() {
    let database = database().await;
    let scope = scope(&database).await;
    let fetcher = Arc::new(SnapshotFetcher {
        calls: AtomicUsize::new(0),
        barrier: None,
        fail: true,
    });
    let repository =
        PgKnowledgeRepository::from_database(&database).with_url_fetcher(fetcher.clone());
    let failed = repository.import_batch(&scope, vec![item()]).await.unwrap();
    assert_eq!(failed.items[0].status, ImportStatus::Failed);
    assert_eq!(
        failed.items[0].error.as_ref().unwrap().code,
        ErrorCode::DependencyUnavailable
    );
    assert!(failed.items[0].source.is_none());
    assert_eq!(
        repository.import_batch(&scope, vec![item()]).await.unwrap(),
        failed
    );
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);
    assert!(repository.list_sources(&scope).await.unwrap().is_empty());
    assert!(
        repository
            .current_release(&scope)
            .await
            .unwrap()
            .knowledge_release_id
            .is_none()
    );
    let snapshots: i64 = sqlx::query_scalar("SELECT count(*) FROM knowledge_url_snapshot_bytes WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3")
        .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(scope.project_id.unwrap().as_uuid())
        .fetch_one(database.pool()).await.unwrap();
    assert_eq!(snapshots, 0);
}
