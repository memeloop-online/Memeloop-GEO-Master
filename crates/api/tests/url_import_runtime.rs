//! URL tools use the same scoped knowledge import and exact-version read path.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use chrono::Utc;
use geo_api::RepositoryHostOps;
use geo_domain::{
    AppError, ErrorCode, FetchedUrlPage, ImportStatus, KnowledgePurpose, KnowledgeRepository,
    MemoryKnowledgeRepository, SourceKind, TenantScope, UrlPageFetcher,
};
use geo_worker::{HostOps, KnowledgeImportUrlRequest, KnowledgeTextReadRequest};
use uuid::Uuid;

#[derive(Debug, Default)]
struct TestPageFetcher {
    calls: AtomicUsize,
}

#[async_trait]
impl UrlPageFetcher for TestPageFetcher {
    async fn fetch(&self, url: &str) -> Result<FetchedUrlPage, AppError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(FetchedUrlPage {
            requested_url: url.to_owned(),
            final_url: url.to_owned(),
            captured_at: Utc::now(),
            raw_bytes: b"<html><p>The warranty lasts two years.</p></html>".to_vec(),
            media_type: "text/html".to_owned(),
            text: "The warranty lasts two years.".to_owned(),
            parser_version: "test-web-parser-v1".to_owned(),
        })
    }
}

fn scope() -> TenantScope {
    TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    )
}

fn request() -> KnowledgeImportUrlRequest {
    KnowledgeImportUrlRequest {
        url: "https://example.com/guide".to_owned(),
        purpose: KnowledgePurpose::Internal,
        idempotency_key: "stable-page-import".to_owned(),
        name: Some("Guide".to_owned()),
    }
}

#[tokio::test]
async fn url_import_replays_the_same_receipt_and_reads_its_exact_scoped_version() {
    let fetcher = Arc::new(TestPageFetcher::default());
    let repository =
        Arc::new(MemoryKnowledgeRepository::default().with_url_fetcher(fetcher.clone()));
    let ops = RepositoryHostOps::new(repository.clone());
    let run_scope = scope();
    let receipt = ops
        .knowledge_import_url(&run_scope, request())
        .await
        .unwrap();
    assert_eq!(receipt.status, ImportStatus::Succeeded);
    assert!(receipt.knowledge_release_id.is_some());
    let read_request = KnowledgeTextReadRequest {
        source_id: receipt.source_id.unwrap(),
        source_version_id: receipt.source_version_id.unwrap(),
    };
    let read = ops
        .knowledge_text_read(&run_scope, read_request.clone())
        .await
        .unwrap();
    assert_eq!(read.source.kind, SourceKind::Url);
    assert_eq!(read.source.purpose, KnowledgePurpose::Internal);
    assert_eq!(read.source.name, "Guide");
    assert_eq!(
        read.source_version.source_version_id,
        read_request.source_version_id
    );
    assert_eq!(read.content.text, "The warranty lasts two years.");
    assert!(!serde_json::to_string(&receipt).unwrap().contains("<html>"));

    assert_eq!(
        ops.knowledge_import_url(&run_scope, request())
            .await
            .unwrap(),
        receipt
    );
    let mut changed = request();
    changed.purpose = KnowledgePurpose::Public;
    let conflict = ops.knowledge_import_url(&run_scope, changed).await.unwrap();
    assert_eq!(conflict.status, ImportStatus::Failed);
    assert_eq!(conflict.error.unwrap().code, ErrorCode::Conflict);
    assert!(conflict.source_version_id.is_none());
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(repository.list_sources(&run_scope).await.unwrap().len(), 1);

    let foreign = TenantScope::new(
        run_scope.operator_id,
        run_scope.tenant_id,
        Some(Uuid::new_v4().into()),
    );
    assert_eq!(
        ops.knowledge_text_read(&foreign, read_request)
            .await
            .unwrap_err()
            .code,
        geo_worker::HostOpErrorCode::NotFound,
    );
    let other = ops.knowledge_import_url(&foreign, request()).await.unwrap();
    assert_ne!(other.source_id, receipt.source_id);
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn url_import_failure_has_no_ready_evidence() {
    let ops = RepositoryHostOps::new(Arc::new(MemoryKnowledgeRepository::default()));
    let receipt = ops.knowledge_import_url(&scope(), request()).await.unwrap();
    assert_eq!(receipt.status, ImportStatus::Failed);
    assert_eq!(receipt.error.unwrap().code, ErrorCode::CapabilityMissing);
    assert!(receipt.source_version_id.is_none());
    assert!(receipt.knowledge_release_id.is_none());
}
