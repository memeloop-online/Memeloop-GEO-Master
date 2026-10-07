//! Explicit integration tests: GEO_TEST_DATABASE_URL must name a disposable database.
use chrono::Utc;
use geo_domain::{
    ContentBlock, ContentBlockKind, ContentCheck, ContentFinding, ContentRepository,
    ContentRevision, InitialSource, InitialSourceKind, InitialSourceVisibility, ProjectCreate,
    ProjectRepository, ProjectSettings, ProjectStartCommand, StructuredDocument, TenantScope,
    hash_idempotency_key, settings_hash, start_request_hash,
};
use geo_persistence::{Database, DatabaseConfig, PgContentRepository, PgProjectRepository};
use uuid::Uuid;

fn revision(asset_id: Uuid, revision: i32, base: Option<Uuid>, title: &str) -> ContentRevision {
    ContentRevision {
        revision_id: Uuid::new_v4(),
        asset_id,
        revision,
        base_revision_id: base,
        derived_from_revision_id: None,
        document: StructuredDocument {
            title: title.into(),
            blocks: vec![ContentBlock {
                block_id: Uuid::new_v4(),
                kind: ContentBlockKind::Paragraph,
                text: "Synthetic text".into(),
                citation_ids: vec![],
                items: vec![],
                rich: None,
            }],
            schema_version: None,
        },
        markdown: title.into(),
        evidence: vec![],
        quotes: vec![],
        findings: vec![],
        created_at: Utc::now(),
    }
}

async fn insert_revision(
    database: &Database,
    scope: &TenantScope,
    execution_id: Uuid,
    row: &ContentRevision,
    body: serde_json::Value,
) {
    sqlx::query(
        "INSERT INTO content_revisions
         (revision_id,operator_id,tenant_id,project_id,execution_id,asset_id,revision,
          body,created_at,derived_from_revision_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(row.revision_id)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(scope.project_id.unwrap().as_uuid())
    .bind(execution_id)
    .bind(row.asset_id)
    .bind(row.revision)
    .bind(body)
    .bind(row.created_at)
    .bind(row.derived_from_revision_id)
    .execute(database.pool())
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn scoped_point_and_paged_exact_child_ignore_unrelated_history() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable test database URL");
    let database = Database::connect_and_migrate(&DatabaseConfig::from_url(url).unwrap())
        .await
        .unwrap();
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'Test')")
        .bind(operator)
        .bind(format!("revision-lookup-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,'Test')",
    )
    .bind(tenant)
    .bind(operator)
    .bind(format!("revision-lookup-{tenant}"))
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
                display_name: "Synthetic revision project".into(),
                settings: ProjectSettings {
                    brand_name: "Example".into(),
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
    let frozen_hash = settings_hash(&project.settings.clone().validate_start().unwrap()).unwrap();
    let accepted = projects
        .start(
            &tenant_scope,
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("revision-lookup-test"),
                request_hash: start_request_hash(project.id, project.revision, &frozen_hash),
                settings_hash: frozen_hash,
                operation_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.id));
    let repository = PgContentRepository::from_database(&database);
    // Only the execution FK is needed for these immutable revision rows; no
    // mutable aggregate read is involved in either point lookup.
    let execution_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO content_executions
         (execution_id,operator_id,tenant_id,project_id,cycle_id,manifest_id,
          manifest_revision,policy_version,input_hash,state)
         VALUES ($1,$2,$3,$4,$5,$6,1,'lookup-test','synthetic', '{}'::jsonb)",
    )
    .bind(execution_id)
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(accepted.cycle_id)
    .bind(accepted.document_manifest.manifest_id)
    .execute(database.pool())
    .await
    .unwrap();
    let asset_id = Uuid::new_v4();
    let base = revision(asset_id, 1, None, "Original");
    insert_revision(
        &database,
        &scope,
        execution_id,
        &base,
        serde_json::to_value(&base).unwrap(),
    )
    .await;

    // Four thousand malformed bodies share this asset but not this base.
    // Reading whole asset history would fail deserialization; the targeted
    // point and child lookups must never load any of them.
    sqlx::query(
        "INSERT INTO content_revisions
         (revision_id,operator_id,tenant_id,project_id,execution_id,asset_id,revision,body,created_at)
         SELECT gen_random_uuid(),$1,$2,$3,$4,$5,n,'{}'::jsonb,now()
         FROM generate_series(100,4099) AS n",
    )
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(execution_id)
    .bind(asset_id)
    .execute(database.pool())
    .await
    .unwrap();

    let wrong_document = revision(asset_id, 2, Some(base.revision_id), "Different document");
    let mut child = revision(asset_id, 90, Some(base.revision_id), "Exact document");
    // Keep the same block identity across the one historical row and the
    // requested document: matching is full structured equality, not title.
    child.document.blocks[0].block_id = wrong_document.document.blocks[0].block_id;
    let mut legacy = serde_json::to_value(&child).unwrap();
    let block = &mut legacy["document"]["blocks"][0];
    block.as_object_mut().unwrap().remove("citation_ids");
    block.as_object_mut().unwrap().remove("items");
    block.as_object_mut().unwrap().remove("rich");
    legacy["document"]
        .as_object_mut()
        .unwrap()
        .remove("schema_version");
    for revision_number in 2..=70 {
        let mut candidate = wrong_document.clone();
        candidate.revision_id = Uuid::new_v4();
        candidate.revision = revision_number;
        insert_revision(
            &database,
            &scope,
            execution_id,
            &candidate,
            serde_json::to_value(&candidate).unwrap(),
        )
        .await;
    }
    // Different *full* document with the same title cannot match.
    let mut near = child.clone();
    near.revision_id = Uuid::new_v4();
    near.revision = 71;
    near.document.blocks[0].text = "Changed block text".into();
    insert_revision(
        &database,
        &scope,
        execution_id,
        &near,
        serde_json::to_value(&near).unwrap(),
    )
    .await;
    insert_revision(&database, &scope, execution_id, &child, legacy).await;
    let later = revision(asset_id, 91, Some(base.revision_id), "Exact document");
    let mut later = later;
    later.document = child.document.clone();
    insert_revision(
        &database,
        &scope,
        execution_id,
        &later,
        serde_json::to_value(&later).unwrap(),
    )
    .await;
    let mut derived = revision(asset_id, 92, None, "Copy-on-write child");
    derived.derived_from_revision_id = Some(child.revision_id);
    insert_revision(
        &database,
        &scope,
        execution_id,
        &derived,
        serde_json::to_value(&derived).unwrap(),
    )
    .await;

    let finding = ContentFinding {
        finding_id: Uuid::new_v4(),
        code: "synthetic_check".into(),
        block_id: None,
        evidence: vec![],
        detail: "Synthetic check finding".into(),
        blocking: false,
    };
    let check = ContentCheck {
        check_id: Uuid::new_v4(),
        revision_id: child.revision_id,
        findings: vec![finding.clone()],
        created_at: Utc::now(),
    };
    sqlx::query(
        "INSERT INTO content_checks
         (check_id,operator_id,tenant_id,project_id,execution_id,revision_id,body,created_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(check.check_id)
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(execution_id)
    .bind(child.revision_id)
    .bind(serde_json::to_value(&check).unwrap())
    .bind(check.created_at)
    .execute(database.pool())
    .await
    .unwrap();

    let found = repository
        .find_exact_child_revision(&scope, asset_id, base.revision_id, &child.document)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.revision_id, child.revision_id);
    assert_eq!(found.findings, vec![finding.clone()]);
    assert_eq!(found.document, child.document);
    let point = repository
        .get_revision(&scope, asset_id, child.revision_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(point.findings, vec![finding]);
    assert_eq!(point.document, child.document);
    assert_eq!(
        repository
            .find_exact_child_revision(&scope, asset_id, child.revision_id, &derived.document)
            .await
            .unwrap()
            .unwrap()
            .revision_id,
        derived.revision_id
    );
    // A damaged unrelated revision in the aggregate must not be decoded by
    // the three metadata reads. The immutable revision/check tables stay valid.
    let item_id = Uuid::new_v4();
    let snapshot = serde_json::json!({
        "execution": {
            "execution_id": execution_id,
            "project_id": project.id,
            "cycle_id": accepted.cycle_id,
            "manifest_id": accepted.document_manifest.manifest_id,
            "manifest_revision": 1,
            "policy_version": "lookup-test",
            "input_hash": "synthetic",
            "status": "running",
            "expected_count": 1,
            "coverage": {
                "total": 1, "ready": 1, "blocked": 0, "deferred": 0,
                "not_applicable": 0, "cancelled": 0, "incomplete": 0
            },
            "handoff_id": null
        },
        "items": [{
            "item_id": item_id,
            "execution_id": execution_id,
            "document_key": "synthetic",
            "branch_key": "synthetic",
            "input_hash": "synthetic",
            "planning_state": geo_domain::DocumentManifestItemState::Planned,
            "planning_reason": null,
            "status": "ready",
            "reason": null,
            "source_version_refs": [],
            "brief": null,
            "asset_id": asset_id,
            "current_revision_id": child.revision_id,
            "ready_revision_id": child.revision_id,
            "steps": []
        }],
        "assets": [{
            "asset_id": asset_id,
            "execution_id": execution_id,
            "item_id": item_id,
            "current_revision_id": child.revision_id,
            "created_at": Utc::now()
        }],
        "revisions": [{"broken_unrelated_revision": true}],
        "checks": [],
        "handoff": null,
        "handoffs": []
    });
    sqlx::query("UPDATE content_executions SET state=$1 WHERE execution_id=$2")
        .bind(snapshot)
        .bind(execution_id)
        .execute(database.pool())
        .await
        .unwrap();
    assert_eq!(
        repository
            .get_execution(&scope, execution_id)
            .await
            .unwrap()
            .unwrap()
            .execution_id,
        execution_id
    );
    assert_eq!(
        repository
            .get_item(&scope, execution_id, item_id)
            .await
            .unwrap()
            .unwrap()
            .item_id,
        item_id
    );
    assert_eq!(
        repository
            .get_asset(&scope, asset_id)
            .await
            .unwrap()
            .unwrap()
            .current_revision_id,
        child.revision_id
    );
    assert!(
        repository
            .get_item(&scope, execution_id, Uuid::new_v4())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        repository
            .get_revision(&scope, asset_id, child.revision_id)
            .await
            .unwrap()
            .unwrap()
            .revision_id,
        child.revision_id
    );
    assert!(
        repository
            .get_revision(&scope, Uuid::new_v4(), child.revision_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repository
            .find_exact_child_revision(&scope, Uuid::new_v4(), base.revision_id, &child.document)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repository
            .find_exact_child_revision(&scope, asset_id, Uuid::new_v4(), &child.document)
            .await
            .unwrap()
            .is_none()
    );
    let wrong_scope = TenantScope::new(operator.into(), Uuid::new_v4().into(), Some(project.id));
    assert!(
        repository
            .get_execution(&wrong_scope, execution_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repository
            .get_item(&wrong_scope, execution_id, item_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repository
            .get_asset(&wrong_scope, asset_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repository
            .get_revision(&wrong_scope, asset_id, child.revision_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repository
            .find_exact_child_revision(&wrong_scope, asset_id, base.revision_id, &child.document)
            .await
            .unwrap()
            .is_none()
    );
    let wrong_project =
        TenantScope::new(operator.into(), tenant.into(), Some(Uuid::new_v4().into()));
    assert!(
        repository
            .get_execution(&wrong_project, execution_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repository
            .get_asset(&wrong_project, asset_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repository
            .get_revision(&wrong_project, asset_id, child.revision_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repository
            .find_exact_child_revision(&wrong_project, asset_id, base.revision_id, &child.document)
            .await
            .unwrap()
            .is_none()
    );
}
