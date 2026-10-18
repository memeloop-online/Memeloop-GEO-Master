//! Exact revision delivery shares the existing content and source lifecycle.
use chrono::Utc;
use geo_domain::{
    AppError, ContentBlock, ContentBlockKind, ContentBrief, ContentEvidence, ContentRepository,
    ContentRevision, ContentStep, DocumentManifestPlanRequest, ErrorCode, EvidenceRef, ImportItem,
    InitialSource, InitialSourceKind, InitialSourceVisibility, KnowledgePurpose,
    KnowledgeRepository, PartnerAdminRepository, PartnerPrincipal, PartnerRepository,
    ProjectCreate, ProjectRepository, ProjectSettings, ProjectStartCommand,
    RICH_GENERATION_POLICY_VERSION, SourceKind, StructuredDocument, TenantScope,
    hash_idempotency_key, settings_hash, start_request_hash,
};
use geo_persistence::{
    Database, DatabaseConfig, PgContentRepository, PgKnowledgeRepository, PgPartnerAdminRepository,
    PgPartnerRepository, PgProjectRepository,
};
use uuid::Uuid;

fn code<T>(result: Result<T, AppError>) -> ErrorCode {
    match result {
        Ok(_) => panic!("expected failure"),
        Err(error) => error.code,
    }
}

struct Fixture {
    database: Database,
    scope: TenantScope,
    partners: PgPartnerRepository,
    admin: PgPartnerAdminRepository,
    content: PgContentRepository,
    principal: PartnerPrincipal,
    revision: ContentRevision,
    source: Uuid,
}

async fn fixture() -> Fixture {
    let database = Database::connect_and_migrate(
        &DatabaseConfig::from_url(
            std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database"),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    sqlx::query("INSERT INTO operators(operator_id,slug,display_name) VALUES($1,$2,'Synthetic')")
        .bind(operator)
        .bind(format!("partner-export-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants(operator_id,tenant_id,slug,display_name) VALUES($1,$2,$3,'Synthetic')",
    )
    .bind(operator)
    .bind(tenant)
    .bind(format!("partner-export-{tenant}"))
    .execute(database.pool())
    .await
    .unwrap();
    let tenant_scope = TenantScope::new(operator.into(), tenant.into(), None);
    let projects = PgProjectRepository::from_database(&database);
    let project = projects
        .create(
            &tenant_scope,
            ProjectCreate {
                slug: None,
                display_name: "Synthetic export project".into(),
                settings: ProjectSettings {
                    brand_name: "Synthetic".into(),
                    market: "global".into(),
                    language: "en".into(),
                    initial_sources: vec![InitialSource {
                        kind: InitialSourceKind::Text,
                        value: "Synthetic public reference".into(),
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
    let frozen = project.settings.clone().validate_start().unwrap();
    let hash = settings_hash(&frozen).unwrap();
    let start = projects
        .start(
            &tenant_scope,
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("synthetic-export-start"),
                request_hash: start_request_hash(project.id, project.revision, &hash),
                settings_hash: hash,
                operation_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.id));
    let knowledge = PgKnowledgeRepository::from_database(&database);
    let imported = knowledge
        .import_batch(
            &scope,
            vec![ImportItem {
                client_item_id: "synthetic-export-source".into(),
                kind: SourceKind::Text,
                name: "Synthetic source".into(),
                purpose: KnowledgePurpose::Public,
                text: Some("Documented product capability.".into()),
                url: None,
                object_id: None,
                knowledge_release_id: None,
            }],
        )
        .await
        .unwrap();
    let source = imported.items[0].source.as_ref().unwrap().source_id;
    let release = imported.items[0]
        .release
        .as_ref()
        .unwrap()
        .knowledge_release_id;
    let detail = knowledge
        .get_source_detail(&scope, source)
        .await
        .unwrap()
        .unwrap();
    let chunk = &detail.chunks[0];
    let mut document_scope = frozen.document_scope.clone();
    document_scope.markets = frozen.effective_markets();
    document_scope.languages = frozen.effective_languages();
    let manifest = knowledge
        .plan_document_manifest(
            &scope,
            DocumentManifestPlanRequest {
                manifest_id: start.document_manifest.manifest_id,
                knowledge_release_id: release,
            },
            document_scope,
        )
        .await
        .unwrap();
    let item = manifest
        .items
        .iter()
        .find(|item| item.source_version_refs.contains(&chunk.source_version_id))
        .unwrap()
        .document_manifest_item_id;
    let content = PgContentRepository::from_database(&database);
    let execution = content
        .start(
            &scope,
            start.cycle_id,
            manifest,
            RICH_GENERATION_POLICY_VERSION,
        )
        .await
        .unwrap();
    let prepare = content
        .claim(
            &scope,
            execution.execution_id,
            item,
            ContentStep::Prepare,
            "synthetic",
            Utc::now(),
            600,
        )
        .await
        .unwrap();
    let reference = EvidenceRef {
        source_version_id: chunk.source_version_id,
        chunk_id: Some(chunk.chunk_id),
        locator: chunk.locator.clone(),
    };
    content
        .complete_prepare(
            &scope,
            &prepare,
            ContentBrief {
                brief_id: Uuid::new_v4(),
                title: "Saved article".into(),
                objective: "Describe".into(),
                evidence: vec![reference.clone()],
                quotes: vec![ContentEvidence {
                    reference,
                    exact_quote: chunk.text.clone(),
                }],
                created_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    let generate = content
        .claim(
            &scope,
            execution.execution_id,
            item,
            ContentStep::Generate,
            "synthetic",
            Utc::now(),
            600,
        )
        .await
        .unwrap();
    let revision = content
        .complete_generate(
            &scope,
            &generate,
            StructuredDocument {
                schema_version: 2,
                title: "Original title".into(),
                blocks: vec![ContentBlock {
                    block_id: Uuid::new_v4(),
                    kind: ContentBlockKind::Paragraph,
                    text: "Saved original article".into(),
                    citation_ids: vec![chunk.chunk_id],
                    items: vec![],
                    rich: None,
                }],
            },
        )
        .await
        .unwrap();
    let partners = PgPartnerRepository::from_database(&database);
    let admin = PgPartnerAdminRepository::from_database(&database);
    let connection = admin.create_connection(&scope).await.unwrap();
    assert!(!connection.content_export);
    let key = partners
        .issue_key(&scope, connection.connection_id, None)
        .await
        .unwrap();
    let principal = partners
        .authenticate(connection.connection_id, key.key_id, &key.secret)
        .await
        .unwrap();
    Fixture {
        database,
        scope,
        partners,
        admin,
        content,
        principal,
        revision,
        source,
    }
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn exact_old_revision_requires_current_grants_sources_and_project_scope() {
    let f = fixture().await;
    let revision_id = f.revision.revision_id;
    assert_eq!(
        code(f.partners.content_export(&f.principal, revision_id).await),
        ErrorCode::Forbidden
    );
    f.admin
        .set_content_export(&f.scope, f.principal.connection_id, true)
        .await
        .unwrap();
    let mut changed = f.revision.document.clone();
    changed.title = "Newer title".into();
    let newer = f
        .content
        .edit(&f.scope, f.revision.asset_id, revision_id, changed)
        .await
        .unwrap();
    let exported = f
        .partners
        .content_export(&f.principal, revision_id)
        .await
        .unwrap();
    assert_eq!(exported.revision, f.revision);
    assert_eq!(exported.language.as_deref(), Some("en"));
    assert!(exported.media.is_empty());
    assert_ne!(exported.revision.revision_id, newer.revision_id);
    assert_eq!(
        code(
            f.partners
                .content_export(&f.principal, Uuid::new_v4())
                .await
        ),
        ErrorCode::NotFound
    );
    for scope in [
        TenantScope::new(Uuid::new_v4().into(), f.scope.tenant_id, f.scope.project_id),
        TenantScope::new(
            f.scope.operator_id,
            Uuid::new_v4().into(),
            f.scope.project_id,
        ),
        TenantScope::new(
            f.scope.operator_id,
            f.scope.tenant_id,
            Some(Uuid::new_v4().into()),
        ),
    ] {
        let foreign = PartnerPrincipal {
            scope,
            ..f.principal.clone()
        };
        assert!(
            f.partners
                .content_export(&foreign, revision_id)
                .await
                .is_err()
        );
    }
    // Restriction accepted now blocks the old bytes without mutating the revision.
    sqlx::query("UPDATE knowledge_sources SET purpose='internal' WHERE source_id=$1")
        .bind(f.source)
        .execute(f.database.pool())
        .await
        .unwrap();
    assert_eq!(
        code(f.partners.content_export(&f.principal, revision_id).await),
        ErrorCode::Conflict
    );
    assert_eq!(
        f.content
            .get_revision(&f.scope, f.revision.asset_id, revision_id)
            .await
            .unwrap()
            .unwrap(),
        f.revision
    );
    sqlx::query("UPDATE knowledge_sources SET purpose='public' WHERE source_id=$1")
        .bind(f.source)
        .execute(f.database.pool())
        .await
        .unwrap();
    f.admin
        .set_content_export(&f.scope, f.principal.connection_id, false)
        .await
        .unwrap();
    assert_eq!(
        code(f.partners.content_export(&f.principal, revision_id).await),
        ErrorCode::Forbidden
    );
    f.admin
        .set_content_export(&f.scope, f.principal.connection_id, true)
        .await
        .unwrap();
    f.partners
        .revoke_key(&f.scope, f.principal.connection_id, f.principal.key_id)
        .await
        .unwrap();
    assert_eq!(
        code(f.partners.content_export(&f.principal, revision_id).await),
        ErrorCode::Forbidden
    );
    let publications: i64 = sqlx::query_scalar("SELECT count(*) FROM distribution_publication_intents WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3")
        .bind(f.scope.operator_id.as_uuid()).bind(f.scope.tenant_id.as_uuid()).bind(f.scope.project_id.unwrap().as_uuid())
        .fetch_one(f.database.pool()).await.unwrap();
    assert_eq!(publications, 0);
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn exact_media_snapshot_survives_later_withdrawal_but_new_download_is_blocked() {
    use geo_domain::{
        ContentMediaRepository, MediaObjectKey, MediaReference, RichContent, RichNode,
        VerifiedImage, sha256_hex,
    };
    use geo_persistence::PgContentMediaRepository;

    let f = fixture().await;
    let session = Uuid::new_v4();
    let object = Uuid::new_v4();
    let bytes = b"synthetic immutable image bytes";
    let hash = sha256_hex(bytes);
    sqlx::query("INSERT INTO knowledge_upload_sessions \
        (upload_session_id,operator_id,tenant_id,project_id,revision,filename,declared_media_type, \
         expected_size,expected_sha256,purpose,state,expires_at,committed_object_id,staging_object_ref) \
        VALUES($1,$2,$3,$4,2,'synthetic.png','image/png',$5,$6,'internal','committed',now()+interval '1 day',$7,'agent-attachment')")
        .bind(session).bind(f.scope.operator_id.as_uuid()).bind(f.scope.tenant_id.as_uuid()).bind(f.scope.project_id.unwrap().as_uuid())
        .bind(bytes.len() as i64).bind(&hash).bind(object).execute(f.database.pool()).await.unwrap();
    sqlx::query("INSERT INTO knowledge_upload_blobs(upload_session_id,content,actual_size,sha256) VALUES($1,$2,$3,$4)")
        .bind(session).bind(bytes.as_slice()).bind(bytes.len() as i64).bind(&hash).execute(f.database.pool()).await.unwrap();
    sqlx::query("INSERT INTO knowledge_stored_objects \
        (object_id,operator_id,tenant_id,project_id,object_version,backend,opaque_key,actual_size,detected_media_type,sha256,state) \
        VALUES($1,$2,$3,$4,1,'postgres_blob',$5,$6,'image/png',$7,'committed')")
        .bind(object).bind(f.scope.operator_id.as_uuid()).bind(f.scope.tenant_id.as_uuid()).bind(f.scope.project_id.unwrap().as_uuid())
        .bind(format!("upload/{session}")).bind(bytes.len() as i64).bind(&hash).execute(f.database.pool()).await.unwrap();
    let media = PgContentMediaRepository::from_database(&f.database);
    let binding = media
        .create_binding(
            &f.scope,
            VerifiedImage {
                key: MediaObjectKey {
                    object_id: object,
                    object_version: 1,
                    sha256: hash.clone(),
                },
                media_type: "image/png".into(),
                byte_len: bytes.len() as u64,
                width: 1,
                height: 1,
            },
        )
        .await
        .unwrap();
    let mut document = f.revision.document.clone();
    document.blocks.push(ContentBlock {
        block_id: Uuid::new_v4(),
        kind: ContentBlockKind::Rich,
        text: String::new(),
        citation_ids: vec![],
        items: vec![],
        rich: Some(RichContent {
            version: 1,
            node: RichNode::Media {
                attrs: MediaReference {
                    object_id: object,
                    object_version: 1,
                    sha256: hash,
                    alt: "Synthetic image".into(),
                    caption: String::new(),
                },
            },
        }),
    });
    let revision = f
        .content
        .edit(
            &f.scope,
            f.revision.asset_id,
            f.revision.revision_id,
            document,
        )
        .await
        .unwrap();
    f.admin
        .set_content_export(&f.scope, f.principal.connection_id, true)
        .await
        .unwrap();
    let downloaded = f
        .partners
        .content_export(&f.principal, revision.revision_id)
        .await
        .unwrap();
    assert_eq!(downloaded.revision.revision_id, revision.revision_id);
    assert_eq!(downloaded.media.len(), 1);
    assert_eq!(downloaded.media[0].bytes, bytes);
    media
        .withdraw_binding(&f.scope, binding.binding_id)
        .await
        .unwrap();
    assert_eq!(
        code(
            f.partners
                .content_export(&f.principal, revision.revision_id)
                .await
        ),
        ErrorCode::Conflict
    );
    assert_eq!(downloaded.media[0].bytes, bytes);
    assert!(
        f.partners
            .content_export(&f.principal, f.revision.revision_id)
            .await
            .is_ok()
    );
}
