//! Disposable PostgreSQL tests: GEO_TEST_DATABASE_URL must name a test server.
use chrono::{Duration, Utc};
use geo_domain::{
    AuthorizePublicationSend, ChannelJobRepository, ChannelSecret, ChannelTargetInput,
    ContentBlock, ContentBlockKind, ContentMediaRepository, ContentRevision, ErrorCode,
    InitialSource, InitialSourceKind, InitialSourceVisibility, MediaObjectKey, MediaReference,
    PlatformPlacement, ProjectCreate, ProjectRepository, ProjectSettings, ProjectStartCommand,
    PublicationSendAuthorizationRepository, PublicationSendDecision, RegisterPublicationSend,
    RichContent, RichNode, StructuredDocument, TenantScope, VerifiedImage, hash_idempotency_key,
    prepare_rich_variant_authorized, settings_hash, sha256_hex, start_request_hash,
};
use geo_persistence::{
    Database, PgChannelJobRepository, PgContentMediaRepository, PgProjectRepository,
    PgPublicationSendAuthorizationRepository,
};
use sqlx::{PgPool, Row};
use uuid::Uuid;

struct Setup {
    database: Database,
    admin: PgPool,
    schema: String,
    scope: TenantScope,
    attempt_id: Uuid,
    binding_id: Uuid,
    registration: RegisterPublicationSend,
    expected: AuthorizePublicationSend,
}

async fn setup() -> Setup {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable test database URL");
    let admin = PgPool::connect(&url).await.unwrap();
    let schema = format!("publication_send_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let options = url
        .parse::<sqlx::postgres::PgConnectOptions>()
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect_with(options)
        .await
        .unwrap();
    let database = Database::from_pool(pool);
    database.migrate().await.unwrap();
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    sqlx::query("INSERT INTO operators(operator_id,slug,display_name) VALUES ($1,$2,'Test')")
        .bind(operator)
        .bind(format!("publication-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants(tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,'Test')",
    )
    .bind(tenant)
    .bind(operator)
    .bind(format!("publication-{tenant}"))
    .execute(database.pool())
    .await
    .unwrap();
    let owner = TenantScope::new(operator.into(), tenant.into(), None);
    let projects = PgProjectRepository::from_database(&database);
    let project = projects
        .create(
            &owner,
            ProjectCreate {
                slug: None,
                display_name: "Test".into(),
                settings: ProjectSettings {
                    brand_name: "Test".into(),
                    market: "global".into(),
                    language: "en".into(),
                    initial_sources: vec![InitialSource {
                        kind: InitialSourceKind::Url,
                        value: "https://example.invalid/source".into(),
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
    let settings = settings_hash(&project.settings.clone().validate_start().unwrap()).unwrap();
    let started = projects
        .start(
            &owner,
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("rich-send"),
                request_hash: start_request_hash(project.id, project.revision, &settings),
                settings_hash: settings,
                operation_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.id));
    let keys = (operator, tenant, project.id.as_uuid());
    let execution = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO content_executions
         (execution_id,operator_id,tenant_id,project_id,cycle_id,manifest_id,
          manifest_revision,policy_version,input_hash,state)
         VALUES ($1,$2,$3,$4,$5,$6,1,'test-rich-v2','fixture','{}')",
    )
    .bind(execution)
    .bind(keys.0)
    .bind(keys.1)
    .bind(keys.2)
    .bind(started.cycle_id)
    .bind(started.document_manifest.manifest_id)
    .execute(database.pool())
    .await
    .unwrap();
    let bytes = b"exact-immutable-test-bytes";
    let hash = sha256_hex(bytes);
    let upload = Uuid::new_v4();
    let object = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO knowledge_upload_sessions
         (upload_session_id,operator_id,tenant_id,project_id,revision,filename,
          declared_media_type,expected_size,expected_sha256,purpose,state,
          expires_at,committed_object_id,staging_object_ref)
         VALUES ($1,$2,$3,$4,2,'image.png','image/png',$5,$6,'internal','committed',
                 now()+interval '1 day',$7,'agent-attachment')",
    )
    .bind(upload)
    .bind(keys.0)
    .bind(keys.1)
    .bind(keys.2)
    .bind(bytes.len() as i64)
    .bind(&hash)
    .bind(object)
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO knowledge_upload_blobs(upload_session_id,content,actual_size,sha256) VALUES ($1,$2,$3,$4)")
        .bind(upload).bind(bytes.as_slice()).bind(bytes.len() as i64).bind(&hash)
        .execute(database.pool()).await.unwrap();
    sqlx::query(
        "INSERT INTO knowledge_stored_objects
         (object_id,operator_id,tenant_id,project_id,object_version,backend,opaque_key,
          actual_size,detected_media_type,sha256,state)
         VALUES ($1,$2,$3,$4,1,'postgres_blob',$5,$6,'image/png',$7,'committed')",
    )
    .bind(object)
    .bind(keys.0)
    .bind(keys.1)
    .bind(keys.2)
    .bind(format!("upload/{upload}"))
    .bind(bytes.len() as i64)
    .bind(&hash)
    .execute(database.pool())
    .await
    .unwrap();
    let image = VerifiedImage {
        key: MediaObjectKey {
            object_id: object,
            object_version: 1,
            sha256: hash.clone(),
        },
        media_type: "image/png".into(),
        byte_len: bytes.len() as u64,
        width: 1,
        height: 1,
    };
    let binding_id = PgContentMediaRepository::from_database(&database)
        .create_binding(&scope, image)
        .await
        .unwrap()
        .binding_id;
    let document = StructuredDocument {
        title: "Rich article".into(),
        schema_version: Some(2),
        blocks: vec![
            ContentBlock {
                block_id: Uuid::new_v4(),
                kind: ContentBlockKind::Rich,
                text: String::new(),
                citation_ids: vec![],
                items: vec![],
                rich: Some(RichContent {
                    version: 1,
                    node: RichNode::Paragraph {
                        content: vec![RichNode::Text {
                            text: "A useful explanation".into(),
                            marks: vec![],
                        }],
                    },
                }),
            },
            ContentBlock {
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
                            alt: "Illustration".into(),
                            caption: "Caption".into(),
                        },
                    },
                }),
            },
        ],
    };
    let revision = ContentRevision {
        revision_id: Uuid::new_v4(),
        asset_id: Uuid::new_v4(),
        revision: 1,
        base_revision_id: None,
        derived_from_revision_id: None,
        markdown: document.markdown(),
        document,
        evidence: vec![],
        quotes: vec![],
        findings: vec![],
        created_at: Utc::now(),
    };
    sqlx::query(
        "INSERT INTO content_revisions
         (revision_id,operator_id,tenant_id,project_id,execution_id,asset_id,
          revision,body,created_at) VALUES ($1,$2,$3,$4,$5,$6,1,$7,$8)",
    )
    .bind(revision.revision_id)
    .bind(keys.0)
    .bind(keys.1)
    .bind(keys.2)
    .bind(execution)
    .bind(revision.asset_id)
    .bind(serde_json::to_value(&revision).unwrap())
    .bind(revision.created_at)
    .execute(database.pool())
    .await
    .unwrap();
    let account_id = Uuid::new_v4();
    let placement = PlatformPlacement {
        platform_id: "generic".into(),
        placement_slot: "primary".into(),
        capability_version: "rich-v2".into(),
        supported_formats: vec!["rich_markdown.v2".into()],
        unavailable_reason: None,
        fixture: false,
    };
    let media = PgContentMediaRepository::from_database(&database)
        .get_binding(&scope, binding_id)
        .await
        .unwrap()
        .unwrap();
    let variant = prepare_rich_variant_authorized(&revision, &placement, &scope, &[media]).unwrap();
    sqlx::query(
        "INSERT INTO distribution_channel_variants
         (variant_id,operator_id,tenant_id,project_id,content_revision_id,body)
         VALUES ($1,$2,$3,$4,$5,$6)",
    )
    .bind(variant.variant_id)
    .bind(keys.0)
    .bind(keys.1)
    .bind(keys.2)
    .bind(revision.revision_id)
    .bind(serde_json::to_value(&variant).unwrap())
    .execute(database.pool())
    .await
    .unwrap();
    let intent_id = Uuid::new_v4();
    let target_id = Uuid::new_v4();
    let attempt_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO content_distribution_requests
         (request_id,operator_id,tenant_id,project_id,schema_version,
          content_revision_id,content_asset_id,platform_id,placement_slot,
          account_id,account_owner_kind,format,idempotency_key_hash,request_hash)
         VALUES ($1,$2,$3,$4,1,$5,$6,'generic','primary',$7,'customer','rich_markdown.v2',$8,$9)",
    )
    .bind(request_id)
    .bind(keys.0)
    .bind(keys.1)
    .bind(keys.2)
    .bind(revision.revision_id)
    .bind(revision.asset_id)
    .bind(account_id)
    .bind("1".repeat(64))
    .bind("2".repeat(64))
    .execute(database.pool())
    .await
    .unwrap();
    let intent = geo_domain::PublicationIntent {
        intent_id,
        project_id: project.id,
        channel_target_id: Uuid::nil(),
        variant_id: variant.variant_id,
        content_revision_id: revision.revision_id,
        platform_id: "generic".into(),
        placement_slot: "primary".into(),
        account_id,
        payload_hash: variant.payload_hash.clone(),
        logical_key: "test-intent".into(),
        verification: geo_domain::IntentVerification::Unknown,
        verification_evidence_id: Some(attempt_id),
        created_at: Utc::now(),
    };
    sqlx::query(
        "INSERT INTO distribution_publication_intents
         (intent_id,operator_id,tenant_id,project_id,origin_request_id,variant_id,
          logical_key,verification,verification_evidence_id,body)
         VALUES ($1,$2,$3,$4,$5,$6,$7,'unknown',$8,$9)",
    )
    .bind(intent_id)
    .bind(keys.0)
    .bind(keys.1)
    .bind(keys.2)
    .bind(request_id)
    .bind(variant.variant_id)
    .bind(&intent.logical_key)
    .bind(attempt_id)
    .bind(serde_json::to_value(&intent).unwrap())
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO distribution_publication_commands
         (command_id,operator_id,tenant_id,project_id,intent_id,origin_request_id,
          payload_hash,fixture,status)
         VALUES ($1,$2,$3,$4,$5,$6,$7,false,'claimed')",
    )
    .bind(target_id)
    .bind(keys.0)
    .bind(keys.1)
    .bind(keys.2)
    .bind(intent_id)
    .bind(request_id)
    .bind(&variant.payload_hash)
    .execute(database.pool())
    .await
    .unwrap();
    let target = geo_domain::ChannelTarget {
        target_id,
        input: ChannelTargetInput::GeneratedPublish {
            content_revision_id: revision.revision_id,
            variant_id: variant.variant_id,
            publication_intent_id: intent_id,
            distribution_target_id: Uuid::nil(),
            origin_request_id: Some(request_id),
            platform: "generic".into(),
            account_id,
            title: variant.title.clone(),
            body: variant.markdown.clone(),
            body_sha256: sha256_hex(variant.markdown.as_bytes()),
            payload_hash: variant.payload_hash.clone(),
            evidence: variant.evidence.clone(),
            rich_payload: variant.rich_payload.clone(),
        },
    };
    sqlx::query(
        "INSERT INTO channel_execution_targets
         (target_id,operator_id,tenant_id,project_id,publication_intent_id,kind,frozen_input)
         VALUES ($1,$2,$3,$4,$5,'publish',$6)",
    )
    .bind(target_id)
    .bind(keys.0)
    .bind(keys.1)
    .bind(keys.2)
    .bind(intent_id)
    .bind(serde_json::to_value(target).unwrap())
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE distribution_publication_commands
         SET materialized_target_id=$1,materialized_at=clock_timestamp()
         WHERE command_id=$1",
    )
    .bind(target_id)
    .execute(database.pool())
    .await
    .unwrap();
    let jobs = PgChannelJobRepository::from_database(&database);
    // Seed the original claimed attempt/intent ledger, not another workflow.
    sqlx::query(
        "INSERT INTO channel_execution_attempts
         (attempt_id,operator_id,tenant_id,project_id,target_id,account_id,target_kind,claimed_at)
         VALUES ($1,$2,$3,$4,$5,$6,'publish',clock_timestamp())",
    )
    .bind(attempt_id)
    .bind(keys.0)
    .bind(keys.1)
    .bind(keys.2)
    .bind(target_id)
    .bind(account_id)
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO distribution_publication_attempts
         (attempt_id,operator_id,tenant_id,project_id,intent_id,command_id)
         VALUES ($1,$2,$3,$4,$5,$6)",
    )
    .bind(attempt_id)
    .bind(keys.0)
    .bind(keys.1)
    .bind(keys.2)
    .bind(intent_id)
    .bind(target_id)
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO distribution_intent_evidence
         (evidence_id,operator_id,tenant_id,project_id,intent_id,result,
          attempt_id,fixture,observed_at)
         VALUES ($1,$2,$3,$4,$5,'unknown',$1,false,clock_timestamp())",
    )
    .bind(attempt_id)
    .bind(keys.0)
    .bind(keys.1)
    .bind(keys.2)
    .bind(intent_id)
    .execute(database.pool())
    .await
    .unwrap();
    let encrypted = ChannelSecret::new(vec![7, 8, 9, 10]);
    jobs.store_publication_binding(&scope, target_id, attempt_id, encrypted.clone())
        .await
        .unwrap();
    let registration = RegisterPublicationSend {
        target_id,
        attempt_id,
        account_id,
        runner_session_id: Uuid::new_v4(),
        encrypted_binding_sha256: sha256_hex(encrypted.encrypted_bytes()),
        send_not_after: Utc::now() + Duration::minutes(4),
    };
    let expected = AuthorizePublicationSend {
        target_id,
        attempt_id,
        account_id,
        runner_session_id: registration.runner_session_id,
        publication_intent_id: intent_id,
        payload_hash: variant.payload_hash,
        encrypted_binding_sha256: registration.encrypted_binding_sha256.clone(),
    };
    Setup {
        database,
        admin,
        schema,
        scope,
        attempt_id,
        binding_id,
        registration,
        expected,
    }
}

impl Setup {
    async fn cleanup(self) {
        self.database.pool().close().await;
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
    }
    fn repo(&self) -> PgPublicationSendAuthorizationRepository {
        PgPublicationSendAuthorizationRepository::from_database(&self.database)
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL in GEO_TEST_DATABASE_URL"]
async fn concurrent_authorization_grants_once_and_replay_never_means_published() {
    let test = setup().await;
    let repo = test.repo();
    assert_eq!(
        repo.authorize_publication_send(&test.scope, &test.expected)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    repo.register_publication_send(&test.scope, &test.registration)
        .await
        .unwrap();
    repo.register_publication_send(&test.scope, &test.registration)
        .await
        .unwrap();
    let (left, right) = tokio::join!(
        repo.authorize_publication_send(&test.scope, &test.expected),
        repo.authorize_publication_send(&test.scope, &test.expected),
    );
    let results = [left.unwrap(), right.unwrap()];
    assert_eq!(
        results
            .iter()
            .filter(|v| matches!(v, PublicationSendDecision::Granted(_)))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|v| matches!(v, PublicationSendDecision::AlreadyConsumed))
            .count(),
        1
    );
    assert_eq!(
        repo.authorize_publication_send(&test.scope, &test.expected)
            .await
            .unwrap(),
        PublicationSendDecision::AlreadyConsumed
    );
    let row = sqlx::query(
        "SELECT send_authorized_at,outcome,received_at FROM channel_execution_attempts
         WHERE attempt_id=$1",
    )
    .bind(test.attempt_id)
    .fetch_one(test.database.pool())
    .await
    .unwrap();
    assert!(
        row.get::<Option<chrono::DateTime<Utc>>, _>("send_authorized_at")
            .is_some()
    );
    assert!(row.get::<Option<serde_json::Value>, _>("outcome").is_none());
    assert!(
        row.get::<Option<chrono::DateTime<Utc>>, _>("received_at")
            .is_none()
    );
    test.cleanup().await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL in GEO_TEST_DATABASE_URL"]
async fn registration_and_callback_are_bound_to_original_scope_session_hash_and_deadline() {
    let test = setup().await;
    let repo = test.repo();
    let mut changed = test.registration.clone();
    changed.encrypted_binding_sha256 = "0".repeat(64);
    assert_eq!(
        repo.register_publication_send(&test.scope, &changed)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    repo.register_publication_send(&test.scope, &test.registration)
        .await
        .unwrap();
    changed = test.registration.clone();
    changed.runner_session_id = Uuid::new_v4();
    assert_eq!(
        repo.register_publication_send(&test.scope, &changed)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    for change in 0..5 {
        let mut expected = test.expected.clone();
        match change {
            0 => expected.runner_session_id = Uuid::new_v4(),
            1 => expected.payload_hash = "other".into(),
            2 => expected.encrypted_binding_sha256 = "0".repeat(64),
            3 => expected.account_id = Uuid::new_v4(),
            _ => expected.publication_intent_id = Uuid::new_v4(),
        }
        assert_eq!(
            repo.authorize_publication_send(&test.scope, &expected)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Conflict
        );
    }
    let other = TenantScope::new(
        test.scope.operator_id,
        Uuid::new_v4().into(),
        test.scope.project_id,
    );
    assert_eq!(
        repo.authorize_publication_send(&other, &test.expected)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    sqlx::query("UPDATE channel_execution_attempts SET send_not_after=clock_timestamp()-interval '1 second' WHERE attempt_id=$1")
        .bind(test.attempt_id).execute(test.database.pool()).await.unwrap_err();
    // A late request is rejected by database time, not a caller-provided clock.
    let expired = setup().await;
    let expired_repo = expired.repo();
    let mut registration = expired.registration.clone();
    registration.send_not_after = Utc::now() + Duration::seconds(3);
    expired_repo
        .register_publication_send(&expired.scope, &registration)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    assert_eq!(
        expired_repo
            .authorize_publication_send(&expired.scope, &expired.expected)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    expired.cleanup().await;
    let finished = setup().await;
    sqlx::query(
        "UPDATE channel_execution_attempts SET outcome='{}',received_at=clock_timestamp()
         WHERE attempt_id=$1",
    )
    .bind(finished.attempt_id)
    .execute(finished.database.pool())
    .await
    .unwrap();
    assert_eq!(
        finished
            .repo()
            .register_publication_send(&finished.scope, &finished.registration)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    finished.cleanup().await;
    test.cleanup().await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL in GEO_TEST_DATABASE_URL"]
async fn withdrawal_and_grant_are_ordered_by_project_lock() {
    let test = setup().await;
    let repo = test.repo();
    repo.register_publication_send(&test.scope, &test.registration)
        .await
        .unwrap();
    let media = PgContentMediaRepository::from_database(&test.database);
    media
        .withdraw_binding(&test.scope, test.binding_id)
        .await
        .unwrap();
    assert_eq!(
        repo.authorize_publication_send(&test.scope, &test.expected)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    test.cleanup().await;
    let after = setup().await;
    let repo = after.repo();
    repo.register_publication_send(&after.scope, &after.registration)
        .await
        .unwrap();
    assert!(matches!(
        repo.authorize_publication_send(&after.scope, &after.expected)
            .await
            .unwrap(),
        PublicationSendDecision::Granted(_)
    ));
    PgContentMediaRepository::from_database(&after.database)
        .withdraw_binding(&after.scope, after.binding_id)
        .await
        .unwrap();
    assert_eq!(
        repo.authorize_publication_send(&after.scope, &after.expected)
            .await
            .unwrap(),
        PublicationSendDecision::AlreadyConsumed
    );
    after.cleanup().await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL in GEO_TEST_DATABASE_URL"]
async fn concurrent_withdrawal_and_authorization_have_one_commit_order() {
    let test = setup().await;
    let repo = test.repo();
    repo.register_publication_send(&test.scope, &test.registration)
        .await
        .unwrap();
    let media = PgContentMediaRepository::from_database(&test.database);
    let (send, withdraw) = tokio::join!(
        repo.authorize_publication_send(&test.scope, &test.expected),
        media.withdraw_binding(&test.scope, test.binding_id),
    );
    withdraw.unwrap();
    match send {
        Ok(PublicationSendDecision::Granted(_)) => {
            // Grant committed before withdrawal. Its right to a single
            // already-started execution is not retrospectively removed.
            assert_eq!(
                repo.authorize_publication_send(&test.scope, &test.expected)
                    .await
                    .unwrap(),
                PublicationSendDecision::AlreadyConsumed,
            );
        }
        Err(error) => {
            // Withdrawal committed first, so no permission ever existed.
            assert_eq!(error.code, ErrorCode::Conflict);
            let authorized: Option<chrono::DateTime<Utc>> = sqlx::query_scalar(
                "SELECT send_authorized_at FROM channel_execution_attempts WHERE attempt_id=$1",
            )
            .bind(test.attempt_id)
            .fetch_one(test.database.pool())
            .await
            .unwrap();
            assert!(authorized.is_none());
        }
        other => panic!("unexpected concurrent send decision: {other:?}"),
    }
    test.cleanup().await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL in GEO_TEST_DATABASE_URL"]
async fn altered_committed_attachment_bytes_cannot_be_authorized() {
    let test = setup().await;
    let repo = test.repo();
    repo.register_publication_send(&test.scope, &test.registration)
        .await
        .unwrap();
    let binding = PgContentMediaRepository::from_database(&test.database)
        .get_binding(&test.scope, test.binding_id)
        .await
        .unwrap()
        .unwrap();
    let upload: Uuid = sqlx::query_scalar(
        "SELECT upload_session_id FROM knowledge_upload_sessions
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND committed_object_id=$4",
    )
    .bind(test.scope.operator_id.as_uuid())
    .bind(test.scope.tenant_id.as_uuid())
    .bind(test.scope.project_id.unwrap().as_uuid())
    .bind(binding.image.key.object_id)
    .fetch_one(test.database.pool())
    .await
    .unwrap();
    sqlx::query("UPDATE knowledge_upload_blobs SET content=$1 WHERE upload_session_id=$2")
        .bind(b"changed-immutable-test-bytes".as_slice())
        .bind(upload)
        .execute(test.database.pool())
        .await
        .unwrap();
    assert_eq!(
        repo.authorize_publication_send(&test.scope, &test.expected)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let sent: Option<chrono::DateTime<Utc>> = sqlx::query_scalar(
        "SELECT send_authorized_at FROM channel_execution_attempts WHERE attempt_id=$1",
    )
    .bind(test.attempt_id)
    .fetch_one(test.database.pool())
    .await
    .unwrap();
    assert!(sent.is_none());
    test.cleanup().await;
}
