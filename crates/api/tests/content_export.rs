use std::{
    io::{Cursor, Read},
    sync::Arc,
};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode, header},
};
use chrono::Utc;
use geo_api::{AppState, EventBus, MemoryIdempotencyStore, MemoryOperationStore, router};
use geo_domain::{
    ChunkLocator, ContentBlock, ContentBlockKind, ContentBrief, ContentRepository, ContentStep,
    DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, DocumentManifest, DocumentManifestCoverage,
    DocumentManifestItem, DocumentManifestItemState, DocumentManifestState, EvidenceRef,
    InitialSource, InitialSourceKind, InitialSourceVisibility, KnowledgePurpose,
    MemoryAuthRepository, MemoryContentMediaRepository, MemoryContentRepository,
    MemoryKnowledgeRepository, MemoryProjectRepository, ProjectCreate, ProjectId, ProjectSettings,
    RICH_GENERATION_POLICY_VERSION, StructuredDocument, TenantScope, UploadSessionCommand,
};
use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;
use uuid::Uuid;
use zip::ZipArchive;

struct Fixture {
    state: AppState,
    content: Arc<MemoryContentRepository>,
    app: Router,
    scope: TenantScope,
    project: ProjectId,
    cookie: String,
    csrf: String,
    execution_id: Uuid,
    item_id: Uuid,
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    csrf: Option<&str>,
    payload: Value,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost:8080")
        .header("origin", "http://localhost:5173")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    if let Some(csrf) = csrf {
        builder = builder.header("x-csrf-token", csrf);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(payload.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap()
        .to_vec();
    (status, headers, body)
}

async fn fixture() -> Fixture {
    let projects = Arc::new(MemoryProjectRepository::default());
    let knowledge = Arc::new(MemoryKnowledgeRepository::default());
    let media = Arc::new(MemoryContentMediaRepository::with_knowledge_repository(
        knowledge.clone(),
    ));
    let content = Arc::new(MemoryContentRepository::with_media_repository(
        media.clone(),
    ));
    let state = AppState::with_stores_and_auth_and_projects_and_knowledge(
        Arc::new(MemoryOperationStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
        Arc::new(MemoryAuthRepository::development_with_password(
            "synthetic-password",
        )),
        projects,
        knowledge,
        EventBus::default(),
        false,
    )
    .with_content_repository(content.clone())
    .with_content_media_repository(media);
    let project = state
        .project_repository()
        .create(
            &TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None),
            ProjectCreate {
                slug: None,
                display_name: "Synthetic export".into(),
                settings: ProjectSettings {
                    brand_name: "Synthetic".into(),
                    market: "US".into(),
                    language: "en".into(),
                    initial_sources: vec![InitialSource {
                        kind: InitialSourceKind::Text,
                        value: "Synthetic source".into(),
                        visibility: InitialSourceVisibility::Public,
                        version_ref: None,
                        content_hash: None,
                    }],
                    ..ProjectSettings::default()
                },
            },
        )
        .await
        .unwrap()
        .id;
    let scope = TenantScope::new(
        DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Some(project),
    );
    let manifest_id = Uuid::new_v4();
    let release_id = Uuid::new_v4();
    let item_id = Uuid::new_v4();
    let source_version_id = Uuid::new_v4();
    let manifest = DocumentManifest {
        manifest_id,
        operator_id: scope.operator_id,
        tenant_id: scope.tenant_id,
        project_id: project,
        revision: 1,
        knowledge_release_id: release_id,
        planner_version: "synthetic".into(),
        state: DocumentManifestState::Ready,
        sealed: true,
        expected_count: Some(1),
        scope_hash: "synthetic".into(),
        items: vec![DocumentManifestItem {
            document_manifest_item_id: item_id,
            manifest_id,
            knowledge_release_id: release_id,
            document_key: "synthetic-guide".into(),
            content_type: "guide".into(),
            product_id: None,
            market: "global".into(),
            language: "en".into(),
            state: DocumentManifestItemState::Planned,
            block_reason: None,
            dependency_hash: "synthetic".into(),
            source_version_refs: vec![source_version_id],
        }],
        coverage: DocumentManifestCoverage {
            total: 1,
            planned: 1,
            blocked: 0,
            deferred: 0,
            not_applicable: 0,
        },
    };
    let execution = content
        .start(
            &scope,
            Uuid::new_v4(),
            manifest,
            RICH_GENERATION_POLICY_VERSION,
        )
        .await
        .unwrap();
    let prepare = content
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Prepare,
            "synthetic",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    content
        .complete_prepare(
            &scope,
            &prepare,
            ContentBrief {
                brief_id: Uuid::new_v4(),
                title: "Synthetic guide".into(),
                objective: "Demonstrate archive".into(),
                evidence: vec![EvidenceRef {
                    source_version_id,
                    chunk_id: Some(Uuid::new_v4()),
                    locator: ChunkLocator::Manual {},
                }],
                quotes: vec![],
                created_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    let app = router(state.clone());
    let (status, headers, body) = call(
        &app,
        "POST",
        "/api/v1/auth/login",
        None,
        None,
        json!({"login_name":"demo@localhost","password":"synthetic-password"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cookie = headers[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let csrf = serde_json::from_slice::<Value>(&body).unwrap()["csrf_token"]
        .as_str()
        .unwrap()
        .to_owned();
    Fixture {
        state,
        content,
        app,
        scope,
        project,
        cookie,
        csrf,
        execution_id: execution.execution_id,
        item_id,
    }
}

fn png() -> Vec<u8> {
    let mut encoded = Vec::new();
    PngEncoder::new(&mut encoded)
        .write_image(
            &[241, 19, 44, 7, 223, 61, 20, 38, 252, 196, 14, 29],
            2,
            2,
            ExtendedColorType::Rgb8,
        )
        .unwrap();
    encoded
}

async fn uploaded_image(fixture: &Fixture, bytes: Vec<u8>) -> (Value, Uuid) {
    let digest = hex::encode(Sha256::digest(&bytes));
    let knowledge = fixture.state.knowledge_repository();
    let session = knowledge
        .create_upload_session(
            &fixture.scope,
            UploadSessionCommand {
                filename: "synthetic.png".into(),
                declared_media_type: "image/png".into(),
                expected_size: bytes.len() as u64,
                expected_sha256: digest,
                purpose: KnowledgePurpose::Internal,
            },
        )
        .await
        .unwrap();
    knowledge
        .put_upload_content(&fixture.scope, session.upload_session_id, bytes)
        .await
        .unwrap();
    let (object, _) = knowledge
        .complete_attachment_upload(
            &fixture.scope,
            session.upload_session_id,
            "synthetic-upload",
        )
        .await
        .unwrap();
    let key = json!({
        "object_id":object.object_id,
        "object_version":object.object_version,
        "sha256":object.sha256
    });
    let uri = format!(
        "/api/v1/projects/{}/content-media/bindings?tenant_id={}",
        fixture.project, DEVELOPMENT_TENANT_ID
    );
    let (status, _, body) = call(
        &fixture.app,
        "POST",
        &uri,
        Some(&fixture.cookie),
        Some(&fixture.csrf),
        key.clone(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let binding = serde_json::from_slice::<Value>(&body).unwrap();
    (
        key,
        Uuid::parse_str(binding["binding_id"].as_str().unwrap()).unwrap(),
    )
}

fn document_with_image(key: &Value) -> StructuredDocument {
    let image = json!({
        "type":"media",
        "attrs":{
            "object_id":key["object_id"],
            "object_version":key["object_version"],
            "sha256":key["sha256"],
            "alt":"A <safe> diagram",
            "caption":"Field notes & guide"
        }
    });
    let document = serde_json::from_value(json!({
        "title":"Synthetic guide",
        "schema_version":2,
        "blocks":[
            {
                "block_id":Uuid::new_v4(),
                "kind":"rich","text":"","items":[],"citation_ids":[],
                "rich":{"version":1,"node":image.clone()}
            },
            {
                "block_id":Uuid::new_v4(),
                "kind":"rich","text":"","items":[],"citation_ids":[],
                "rich":{"version":1,"node":image}
            }
        ]
    }))
    .unwrap();
    let document: StructuredDocument = document;
    document.validate(&[]).unwrap();
    document
}

async fn generate(fixture: &Fixture, document: StructuredDocument) -> geo_domain::ContentRevision {
    let lease = fixture
        .content
        .claim(
            &fixture.scope,
            fixture.execution_id,
            fixture.item_id,
            ContentStep::Generate,
            "synthetic",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    fixture
        .content
        .complete_generate(&fixture.scope, &lease, document)
        .await
        .unwrap()
}

fn uri(project: ProjectId, asset_id: Uuid, revision_id: Uuid, format: &str) -> String {
    format!(
        "/api/v1/projects/{project}/contents/{asset_id}/revisions/{revision_id}/export-bundle?format={format}&tenant_id={DEVELOPMENT_TENANT_ID}"
    )
}

fn archive(bytes: &[u8]) -> ZipArchive<Cursor<&[u8]>> {
    ZipArchive::new(Cursor::new(bytes)).unwrap()
}

#[tokio::test]
async fn scoped_bundle_keeps_original_image_bytes_and_historical_revision() {
    let fixture = fixture().await;
    let original_bytes = png();
    let (key, binding_id) = uploaded_image(&fixture, original_bytes.clone()).await;
    let first = generate(&fixture, document_with_image(&key)).await;
    let media_path = format!(
        "media/{}-{}.png",
        key["object_id"].as_str().unwrap(),
        key["object_version"].as_i64().unwrap()
    );
    for format in ["markdown", "html"] {
        let (status, headers, bytes) = call(
            &fixture.app,
            "GET",
            &uri(fixture.project, first.asset_id, first.revision_id, format),
            Some(&fixture.cookie),
            None,
            json!({}),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        assert_eq!(headers[header::CONTENT_TYPE], "application/zip");
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert!(
            headers[header::CONTENT_DISPOSITION]
                .to_str()
                .unwrap()
                .contains(&format!("{}.zip", first.revision_id))
        );
        let mut zip = archive(&bytes);
        assert_eq!(zip.len(), 2, "duplicate references package only one image");
        let mut packaged_image = Vec::new();
        let entry = zip.by_name(&media_path).unwrap();
        assert_eq!(entry.compression(), zip::CompressionMethod::Stored);
        entry
            .take(u64::MAX)
            .read_to_end(&mut packaged_image)
            .unwrap();
        assert_eq!(packaged_image, original_bytes);
        assert_eq!(
            Sha256::digest(&packaged_image),
            Sha256::digest(&original_bytes)
        );
        let document_name = format!(
            "{}.{}",
            first.revision_id,
            if format == "html" { "html" } else { "md" }
        );
        let mut document = String::new();
        zip.by_name(&document_name)
            .unwrap()
            .read_to_string(&mut document)
            .unwrap();
        assert_eq!(document.matches(&media_path).count(), 2);
        assert!(document.contains("Field notes"));
        assert!(document.contains("safe"));
        assert!(!document.contains("data:image/"));
        assert!(!document.contains("https://"));
    }

    let next = fixture
        .content
        .edit(
            &fixture.scope,
            first.asset_id,
            first.revision_id,
            StructuredDocument {
                title: "Without image".into(),
                schema_version: None,
                blocks: vec![ContentBlock {
                    block_id: Uuid::new_v4(),
                    kind: ContentBlockKind::Paragraph,
                    text: "Text only".into(),
                    citation_ids: vec![],
                    items: vec![],
                    rich: None,
                }],
            },
        )
        .await
        .unwrap();
    for format in ["markdown", "html"] {
        let (status, _, bytes) = call(
            &fixture.app,
            "GET",
            &uri(fixture.project, next.asset_id, next.revision_id, format),
            Some(&fixture.cookie),
            None,
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let mut zip = archive(&bytes);
        assert_eq!(zip.len(), 1);
        let mut document = String::new();
        zip.by_name(&format!(
            "{}.{}",
            next.revision_id,
            if format == "html" { "html" } else { "md" }
        ))
        .unwrap()
        .read_to_string(&mut document)
        .unwrap();
        if format == "markdown" {
            assert_eq!(document.as_bytes(), next.markdown.as_bytes());
        } else {
            assert!(document.contains("<p>Text only</p>"));
        }
    }
    let (status, _, _) = call(
        &fixture.app,
        "GET",
        &uri(fixture.project, first.asset_id, first.revision_id, "html"),
        Some(&fixture.cookie),
        None,
        json!({}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "historical revision stays available"
    );
    let other_project = fixture
        .state
        .project_repository()
        .create(
            &TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None),
            ProjectCreate {
                slug: None,
                display_name: "Other synthetic project".into(),
                settings: ProjectSettings {
                    brand_name: "Other".into(),
                    market: "US".into(),
                    language: "en".into(),
                    initial_sources: vec![InitialSource {
                        kind: InitialSourceKind::Text,
                        value: "Other".into(),
                        visibility: InitialSourceVisibility::Public,
                        version_ref: None,
                        content_hash: None,
                    }],
                    ..ProjectSettings::default()
                },
            },
        )
        .await
        .unwrap()
        .id;
    for invalid in [
        uri(other_project, first.asset_id, first.revision_id, "html"),
        uri(fixture.project, Uuid::new_v4(), first.revision_id, "html"),
        uri(fixture.project, first.asset_id, Uuid::new_v4(), "html"),
    ] {
        let (status, _, body) = call(
            &fixture.app,
            "GET",
            &invalid,
            Some(&fixture.cookie),
            None,
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(!body.starts_with(b"PK"));
    }
    let (status, _, _) = call(
        &fixture.app,
        "GET",
        &uri(fixture.project, first.asset_id, first.revision_id, "html"),
        None,
        None,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = call(
        &fixture.app,
        "GET",
        &uri(fixture.project, first.asset_id, first.revision_id, "pdf"),
        Some(&fixture.cookie),
        None,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let withdraw = format!(
        "/api/v1/projects/{}/content-media/bindings/{binding_id}?tenant_id={}",
        fixture.project, DEVELOPMENT_TENANT_ID
    );
    let (status, _, _) = call(
        &fixture.app,
        "DELETE",
        &withdraw,
        Some(&fixture.cookie),
        Some(&fixture.csrf),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for format in ["markdown", "html"] {
        let (status, _, bytes) = call(
            &fixture.app,
            "GET",
            &uri(fixture.project, first.asset_id, first.revision_id, format),
            Some(&fixture.cookie),
            None,
            json!({}),
        )
        .await;
        assert_ne!(status, StatusCode::OK);
        assert!(!bytes.starts_with(b"PK"));
    }
    let (status, _, bytes) = call(
        &fixture.app,
        "GET",
        &uri(fixture.project, next.asset_id, next.revision_id, "markdown"),
        Some(&fixture.cookie),
        None,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(archive(&bytes).len(), 1);
}
