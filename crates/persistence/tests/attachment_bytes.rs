use geo_domain::{
    AttachmentObjectBytes, ErrorCode, KnowledgePurpose, KnowledgeRepository,
    MemoryKnowledgeRepository, StoredObject, TenantScope, UploadSessionCommand, sha256_hex,
};
use geo_persistence::{Database, DatabaseConfig, PgKnowledgeRepository};
use sqlx::PgPool;
use uuid::Uuid;

async fn upload(
    repository: &impl KnowledgeRepository,
    scope: &TenantScope,
    bytes: &[u8],
    filename: &str,
) -> (StoredObject, Uuid) {
    let session = repository
        .create_upload_session(
            scope,
            UploadSessionCommand {
                filename: filename.to_owned(),
                // This value is declared by the uploader, not byte-detected.
                declared_media_type: "image/png".to_owned(),
                expected_size: bytes.len() as u64,
                expected_sha256: sha256_hex(bytes),
                purpose: KnowledgePurpose::Internal,
            },
        )
        .await
        .expect("create upload session");
    repository
        .put_upload_content(scope, session.upload_session_id, bytes.to_vec())
        .await
        .expect("put bytes");
    (
        repository
            .complete_attachment_upload(scope, session.upload_session_id, "attachment")
            .await
            .expect("commit agent attachment")
            .0,
        session.upload_session_id,
    )
}

#[tokio::test]
async fn memory_reads_only_scoped_committed_agent_attachment_snapshots() {
    let repository = MemoryKnowledgeRepository::default();
    let scope = TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    );
    let sibling = TenantScope::new(
        scope.operator_id,
        scope.tenant_id,
        Some(Uuid::new_v4().into()),
    );
    let other_tenant = TenantScope::new(scope.operator_id, Uuid::new_v4().into(), scope.project_id);
    let bytes = b"not actually a PNG";
    let staged = repository
        .create_upload_session(
            &scope,
            UploadSessionCommand {
                filename: "pending.png".to_owned(),
                declared_media_type: "image/png".to_owned(),
                expected_size: bytes.len() as u64,
                expected_sha256: sha256_hex(bytes),
                purpose: KnowledgePurpose::Internal,
            },
        )
        .await
        .unwrap();
    repository
        .put_upload_content(&scope, staged.upload_session_id, bytes.to_vec())
        .await
        .unwrap();
    assert!(
        repository
            .get_attachment_object_bytes(&scope, staged.upload_session_id, 1, &sha256_hex(bytes))
            .await
            .unwrap()
            .is_none()
    );
    let (object, _) = repository
        .complete_attachment_upload(&scope, staged.upload_session_id, "attachment")
        .await
        .unwrap();
    let snapshot = repository
        .get_attachment_object_bytes(
            &scope,
            object.object_id,
            object.object_version,
            &object.sha256,
        )
        .await
        .unwrap()
        .expect("committed attachment");
    assert_eq!(snapshot.object, object);
    assert_eq!(snapshot.bytes, bytes);
    assert_eq!(snapshot.object.detected_media_type, "image/png");
    for rejected_scope in [&sibling, &other_tenant] {
        assert!(
            repository
                .get_attachment_object_bytes(
                    rejected_scope,
                    object.object_id,
                    object.object_version,
                    &object.sha256,
                )
                .await
                .unwrap()
                .is_none()
        );
    }
    for (version, hash) in [
        (object.object_version + 1, object.sha256.as_str()),
        (object.object_version, "different-digest"),
    ] {
        assert!(
            repository
                .get_attachment_object_bytes(&scope, object.object_id, version, hash)
                .await
                .unwrap()
                .is_none()
        );
    }
    let ordinary = repository
        .create_upload_session(
            &scope,
            UploadSessionCommand {
                filename: "ordinary.txt".to_owned(),
                declared_media_type: "text/plain".to_owned(),
                expected_size: bytes.len() as u64,
                expected_sha256: sha256_hex(bytes),
                purpose: KnowledgePurpose::Internal,
            },
        )
        .await
        .unwrap();
    repository
        .put_upload_content(&scope, ordinary.upload_session_id, bytes.to_vec())
        .await
        .unwrap();
    let source = repository
        .complete_upload(&scope, ordinary.upload_session_id, "ordinary")
        .await
        .unwrap();
    let source_object_id = source.source_version.unwrap().object_id.unwrap();
    assert!(
        repository
            .get_attachment_object_bytes(&scope, source_object_id, 1, &sha256_hex(bytes))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repository
            .get_attachment_object_bytes(
                &TenantScope::new(scope.operator_id, scope.tenant_id, None),
                object.object_id,
                object.object_version,
                &object.sha256,
            )
            .await
            .is_err()
    );
}

#[test]
fn attachment_snapshot_rejects_size_digest_and_unbounded_metadata() {
    let scope = TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    );
    let bytes = b"verified";
    let object = StoredObject {
        object_id: Uuid::new_v4(),
        operator_id: scope.operator_id,
        tenant_id: scope.tenant_id,
        project_id: scope.project_id.unwrap(),
        object_version: 1,
        backend: "memory".to_owned(),
        opaque_key: "fixture".to_owned(),
        actual_size: bytes.len() as u64,
        detected_media_type: "image/png".to_owned(),
        sha256: sha256_hex(bytes),
        state: geo_domain::StoredObjectState::Committed,
        created_at: chrono::Utc::now(),
    };
    assert_eq!(
        AttachmentObjectBytes::verified(object.clone(), bytes.to_vec())
            .unwrap()
            .bytes,
        bytes
    );
    for corrupted in [
        StoredObject {
            actual_size: object.actual_size + 1,
            ..object.clone()
        },
        StoredObject {
            sha256: sha256_hex(b"different"),
            ..object.clone()
        },
        StoredObject {
            actual_size: geo_domain::MAX_UPLOAD_BYTES + 1,
            ..object.clone()
        },
    ] {
        assert_eq!(
            AttachmentObjectBytes::verified(corrupted, bytes.to_vec())
                .unwrap_err()
                .code,
            ErrorCode::Conflict
        );
    }
}

async fn seed_project(pool: &PgPool, operator_id: Uuid, tenant_id: Uuid) -> TenantScope {
    let project_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO projects (project_id,operator_id,tenant_id,slug,display_name,status)
         VALUES ($1,$2,$3,$4,'Attachment fixture','active')",
    )
    .bind(project_id)
    .bind(operator_id)
    .bind(tenant_id)
    .bind(format!("attachment-bytes-{project_id}"))
    .execute(pool)
    .await
    .expect("project");
    TenantScope::new(
        operator_id.into(),
        tenant_id.into(),
        Some(project_id.into()),
    )
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn postgres_attachment_bytes_roundtrip_reconnect_scope_and_integrity() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable test database URL");
    let config = DatabaseConfig::from_url(url).expect("valid test database URL");
    let database = Database::connect_and_migrate(&config)
        .await
        .expect("migrations");
    let pool = database.pool();
    let operator_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();
    let other_tenant_id = Uuid::new_v4();
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,$3)")
        .bind(operator_id)
        .bind(format!("attachment-bytes-{operator_id}"))
        .bind("Attachment fixture")
        .execute(pool)
        .await
        .expect("operator");
    for id in [tenant_id, other_tenant_id] {
        sqlx::query(
            "INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,$4)",
        )
        .bind(id)
        .bind(operator_id)
        .bind(format!("attachment-bytes-{id}"))
        .bind("Attachment fixture")
        .execute(pool)
        .await
        .expect("tenant");
    }
    let scope = seed_project(pool, operator_id, tenant_id).await;
    let sibling = seed_project(pool, operator_id, tenant_id).await;
    let other_tenant = seed_project(pool, operator_id, other_tenant_id).await;
    let repository = PgKnowledgeRepository::from_database(&database);
    let bytes = b"original immutable test bytes";
    let staged = repository
        .create_upload_session(
            &scope,
            UploadSessionCommand {
                filename: "pending.png".to_owned(),
                declared_media_type: "image/png".to_owned(),
                expected_size: bytes.len() as u64,
                expected_sha256: sha256_hex(bytes),
                purpose: KnowledgePurpose::Internal,
            },
        )
        .await
        .unwrap();
    repository
        .put_upload_content(&scope, staged.upload_session_id, bytes.to_vec())
        .await
        .unwrap();
    assert!(
        repository
            .get_attachment_object_bytes(&scope, staged.upload_session_id, 1, &sha256_hex(bytes))
            .await
            .unwrap()
            .is_none()
    );
    let (object, session_id) = upload(&repository, &scope, bytes, "picture.png").await;
    let persisted = repository
        .get_attachment_object_bytes(
            &scope,
            object.object_id,
            object.object_version,
            &object.sha256,
        )
        .await
        .expect("bytes query")
        .expect("committed attachment");
    assert_eq!(persisted.object, object);
    assert_eq!(persisted.bytes, bytes);
    assert_eq!(persisted.object.detected_media_type, "image/png");
    drop(repository);
    drop(database);
    let database = Database::connect_and_migrate(&config)
        .await
        .expect("reconnect");
    let repository = PgKnowledgeRepository::from_database(&database);
    assert_eq!(
        repository
            .get_attachment_object_bytes(
                &scope,
                object.object_id,
                object.object_version,
                &object.sha256,
            )
            .await
            .unwrap(),
        Some(persisted)
    );
    for rejected_scope in [&sibling, &other_tenant] {
        assert!(
            repository
                .get_attachment_object_bytes(
                    rejected_scope,
                    object.object_id,
                    object.object_version,
                    &object.sha256,
                )
                .await
                .unwrap()
                .is_none()
        );
    }
    for (version, hash) in [
        (object.object_version + 1, object.sha256.as_str()),
        (object.object_version, "different-digest"),
    ] {
        assert!(
            repository
                .get_attachment_object_bytes(&scope, object.object_id, version, hash)
                .await
                .unwrap()
                .is_none()
        );
    }
    let ordinary = repository
        .create_upload_session(
            &scope,
            UploadSessionCommand {
                filename: "ordinary.txt".to_owned(),
                declared_media_type: "text/plain".to_owned(),
                expected_size: bytes.len() as u64,
                expected_sha256: sha256_hex(bytes),
                purpose: KnowledgePurpose::Internal,
            },
        )
        .await
        .unwrap();
    repository
        .put_upload_content(&scope, ordinary.upload_session_id, bytes.to_vec())
        .await
        .unwrap();
    let source = repository
        .complete_upload(&scope, ordinary.upload_session_id, "ordinary")
        .await
        .unwrap();
    let source_object_id = source.source_version.unwrap().object_id.unwrap();
    assert!(
        repository
            .get_attachment_object_bytes(&scope, source_object_id, 1, &sha256_hex(bytes))
            .await
            .unwrap()
            .is_none()
    );
    // Mutate only this test's isolated blob to confirm actual bytes, rather
    // than its stored digest, govern the result.
    sqlx::query("UPDATE knowledge_upload_blobs SET content=$1 WHERE upload_session_id=$2")
        .bind(b"Xriginal immutable test bytes".as_slice())
        .bind(session_id)
        .execute(database.pool())
        .await
        .expect("tamper fixture");
    assert_eq!(
        repository
            .get_attachment_object_bytes(
                &scope,
                object.object_id,
                object.object_version,
                &object.sha256,
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    sqlx::query(
        "UPDATE knowledge_upload_blobs SET content=$1,actual_size=actual_size+1
         WHERE upload_session_id=$2",
    )
    .bind(bytes.as_slice())
    .bind(session_id)
    .execute(database.pool())
    .await
    .expect("tamper fixture length");
    assert_eq!(
        repository
            .get_attachment_object_bytes(
                &scope,
                object.object_id,
                object.object_version,
                &object.sha256,
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    sqlx::query("DELETE FROM knowledge_upload_blobs WHERE upload_session_id=$1")
        .bind(session_id)
        .execute(database.pool())
        .await
        .expect("remove only this fixture's blob");
    assert_eq!(
        repository
            .get_attachment_object_bytes(
                &scope,
                object.object_id,
                object.object_version,
                &object.sha256,
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
}
