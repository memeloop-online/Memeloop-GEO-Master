//! Run against a disposable PostgreSQL database in GEO_TEST_DATABASE_URL.
use chrono::Utc;
use geo_domain::{
    ContentBlock, ContentBlockKind, ContentBrief, ContentCreateDocument, ContentCreateRequest,
    ContentEvidence, ContentExecutionOrigin, ContentItemStatus, ContentRepository, ContentStep,
    ErrorCode, EvidenceRef, ImportItem, KnowledgePurpose, KnowledgeRepository, ProjectCreate,
    ProjectRepository, ProjectSettings, RICH_GENERATION_POLICY_VERSION, SourceKind,
    StructuredDocument, TenantScope, build_on_demand_manifest,
};
use geo_persistence::{
    Database, DatabaseConfig, PgContentRepository, PgKnowledgeRepository, PgProjectRepository,
};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn draft_on_demand_content_is_scoped_idempotent_and_never_automatically_distributed() {
    let database = Database::connect_and_migrate(
        &DatabaseConfig::from_url(std::env::var("GEO_TEST_DATABASE_URL").unwrap()).unwrap(),
    )
    .await
    .unwrap();
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'Content test')",
    )
    .bind(operator)
    .bind(format!("content-{operator}"))
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,'Content test')")
        .bind(tenant).bind(operator).bind(format!("content-{tenant}"))
        .execute(database.pool()).await.unwrap();
    let tenant_scope = TenantScope::new(operator.into(), tenant.into(), None);
    let projects = PgProjectRepository::from_database(&database);
    let project = projects
        .create(
            &tenant_scope,
            ProjectCreate {
                slug: Some(format!("content-{tenant}")),
                display_name: "On-demand content".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.id));
    let knowledge = PgKnowledgeRepository::from_database(&database);
    let imported = knowledge
        .import_batch(
            &scope,
            (0..3)
                .map(|index| ImportItem {
                    client_item_id: format!("source-{index}"),
                    kind: SourceKind::Text,
                    name: format!("Public reference {index}"),
                    purpose: KnowledgePurpose::Public,
                    text: Some(format!("Documented capability number {index}.")),
                    url: None,
                    object_id: None,
                    knowledge_release_id: None,
                })
                .collect(),
        )
        .await
        .unwrap();
    let mut details = Vec::new();
    for result in &imported.items {
        details.push(
            knowledge
                .get_source_detail(&scope, result.source.as_ref().unwrap().source_id)
                .await
                .unwrap()
                .unwrap(),
        );
    }
    let release = knowledge
        .get_release(
            &scope,
            knowledge
                .current_release(&scope)
                .await
                .unwrap()
                .knowledge_release_id
                .unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    let selected = details
        .iter()
        .take(2)
        .map(|detail| detail.chunks[0].source_version_id)
        .collect::<Vec<_>>();
    let request = ContentCreateRequest {
        request_id: Uuid::new_v4(),
        topic: "Documented capabilities".into(),
        source_version_ids: selected.clone(),
        documents: vec![
            ContentCreateDocument {
                key: "article".into(),
                content_type: "article".into(),
                title: Some("Capabilities".into()),
                instructions: None,
            },
            ContentCreateDocument {
                key: "faq".into(),
                content_type: "faq".into(),
                title: None,
                instructions: Some("Answer common questions".into()),
            },
        ],
        language: "en".into(),
        market: None,
        audience: None,
    };
    let manifest = knowledge
        .save_on_demand_manifest(
            &scope,
            build_on_demand_manifest(&scope, &request, &release).unwrap(),
        )
        .await
        .unwrap();
    assert!(
        manifest
            .items
            .iter()
            .all(|item| item.source_version_refs == selected)
    );
    let repository = PgContentRepository::from_database(&database);
    let replica = PgContentRepository::from_database(&database);
    let (first, duplicate) = tokio::join!(
        repository.start_on_demand(
            &scope,
            request.clone(),
            manifest.clone(),
            RICH_GENERATION_POLICY_VERSION
        ),
        replica.start_on_demand(
            &scope,
            request.clone(),
            manifest.clone(),
            RICH_GENERATION_POLICY_VERSION
        ),
    );
    let execution = first.unwrap();
    assert_eq!(execution, duplicate.unwrap());
    assert_eq!(execution.expected_count, 2);
    assert!(matches!(
        execution.origin,
        ContentExecutionOrigin::OnDemand { .. }
    ));
    assert_eq!(
        repository
            .start_on_demand(&scope, request.clone(), manifest.clone(), "changed-policy")
            .await
            .unwrap(),
        execution
    );
    let mut changed = request.clone();
    changed.topic = "Different task".into();
    assert_eq!(
        repository
            .start_on_demand(
                &scope,
                changed.clone(),
                manifest.clone(),
                RICH_GENERATION_POLICY_VERSION
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        knowledge
            .save_on_demand_manifest(
                &scope,
                build_on_demand_manifest(&scope, &changed, &release).unwrap()
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let other_project = projects
        .create(
            &tenant_scope,
            ProjectCreate {
                slug: Some(format!("other-{tenant}")),
                display_name: "Other content".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    let other_scope = TenantScope::new(operator.into(), tenant.into(), Some(other_project.id));
    assert!(
        replica
            .get_execution(&other_scope, execution.execution_id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        knowledge
            .save_on_demand_manifest(&other_scope, manifest.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Forbidden
    );
    let mut cross_project = manifest.clone();
    cross_project.manifest_id = Uuid::new_v4();
    cross_project.project_id = other_project.id;
    for item in &mut cross_project.items {
        item.manifest_id = cross_project.manifest_id;
    }
    assert_eq!(
        knowledge
            .save_on_demand_manifest(&other_scope, cross_project)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );

    let tick = Utc::now();
    assert!(
        repository
            .scan_running_after(None, tick, 1000)
            .await
            .unwrap()
            .iter()
            .any(|candidate| candidate.execution_id == execution.execution_id)
    );
    let dispatch = repository
        .try_claim_dispatch(
            &scope,
            execution.execution_id,
            tick,
            chrono::Duration::minutes(5),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        repository
            .renew_dispatch(&dispatch, tick, chrono::Duration::minutes(5))
            .await
            .unwrap()
    );
    assert!(
        repository
            .release_dispatch(&dispatch, tick, chrono::Duration::zero())
            .await
            .unwrap()
    );
    for item in replica
        .list_items(&scope, execution.execution_id)
        .await
        .unwrap()
    {
        // The first asset completes while still a draft project. Pausing
        // automatic operations also permits the second requested branch.
        if item.document_key == "faq" {
            sqlx::query("UPDATE projects SET status='paused' WHERE project_id=$1")
                .bind(project.id.as_uuid())
                .execute(database.pool())
                .await
                .unwrap();
            assert!(
                repository
                    .scan_running_after(None, tick, 1000)
                    .await
                    .unwrap()
                    .iter()
                    .any(|candidate| candidate.execution_id == execution.execution_id)
            );
        }
        let chunk = &details[0].chunks[0];
        let reference = EvidenceRef {
            source_version_id: chunk.source_version_id,
            chunk_id: Some(chunk.chunk_id),
            locator: chunk.locator.clone(),
        };
        let prepare = repository
            .claim(
                &scope,
                execution.execution_id,
                item.item_id,
                ContentStep::Prepare,
                "test",
                Utc::now(),
                60,
            )
            .await
            .unwrap();
        repository
            .complete_prepare(
                &scope,
                &prepare,
                ContentBrief {
                    brief_id: Uuid::new_v4(),
                    title: item.document_key.clone(),
                    objective: request.topic.clone(),
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
        let generate = repository
            .claim(
                &scope,
                execution.execution_id,
                item.item_id,
                ContentStep::Generate,
                "test",
                Utc::now(),
                60,
            )
            .await
            .unwrap();
        let revision = repository
            .complete_generate(
                &scope,
                &generate,
                StructuredDocument {
                    schema_version: 2,
                    title: item.document_key.clone(),
                    blocks: vec![ContentBlock {
                        block_id: Uuid::new_v4(),
                        kind: ContentBlockKind::Paragraph,
                        text: chunk.text.clone(),
                        citation_ids: vec![chunk.chunk_id],
                        items: vec![],
                        rich: None,
                    }],
                },
            )
            .await
            .unwrap();
        let check = repository
            .claim(
                &scope,
                execution.execution_id,
                item.item_id,
                ContentStep::Check,
                "test",
                Utc::now(),
                60,
            )
            .await
            .unwrap();
        assert_eq!(
            repository
                .complete_check(&scope, &check, vec![])
                .await
                .unwrap()
                .status,
            ContentItemStatus::Ready
        );
        assert_eq!(
            replica
                .get_revision(&scope, revision.asset_id, revision.revision_id)
                .await
                .unwrap()
                .unwrap()
                .document,
            revision.document
        );
    }
    let handoff = repository
        .close(&scope, execution.execution_id)
        .await
        .unwrap();
    assert_eq!(handoff.coverage.ready, 2);
    assert_eq!(replica.list_project_assets(&scope).await.unwrap().len(), 2);
    // Even an active project cannot route a closed on-demand run into stage 2.
    sqlx::query("UPDATE projects SET status='active' WHERE project_id=$1")
        .bind(project.id.as_uuid())
        .execute(database.pool())
        .await
        .unwrap();
    assert!(
        repository
            .scan_closed_after(None, Utc::now(), 1000)
            .await
            .unwrap()
            .iter()
            .all(|candidate| candidate.execution_id != execution.execution_id)
    );
    assert!(
        repository
            .try_claim_dispatch(
                &scope,
                execution.execution_id,
                Utc::now(),
                chrono::Duration::minutes(5)
            )
            .await
            .unwrap()
            .is_none()
    );
    let counts: (i64,i64,i64,i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM optimization_cycles WHERE project_id=$1), \
                (SELECT count(*) FROM content_executions WHERE project_id=$1 AND cycle_id IS NULL AND origin_kind='on_demand'), \
                (SELECT count(*) FROM distribution_execution_manifests WHERE project_id=$1), \
                (SELECT count(*) FROM distribution_publication_commands WHERE project_id=$1)",
    ).bind(project.id.as_uuid()).fetch_one(database.pool()).await.unwrap();
    assert_eq!(counts, (0, 1, 0, 0));
    assert!(
        projects
            .get(&tenant_scope, project.id)
            .await
            .unwrap()
            .unwrap()
            .settings
            .brand_name
            .is_empty()
    );

    let mut interrupted = request.clone();
    interrupted.request_id = Uuid::new_v4();
    let pending_manifest = knowledge
        .save_on_demand_manifest(
            &scope,
            build_on_demand_manifest(&scope, &interrupted, &release).unwrap(),
        )
        .await
        .unwrap();
    sqlx::query("UPDATE knowledge_sources SET state='removed' WHERE source_id=$1")
        .bind(imported.items[1].source.as_ref().unwrap().source_id)
        .execute(database.pool())
        .await
        .unwrap();
    assert_eq!(
        repository
            .start_on_demand(
                &scope,
                interrupted.clone(),
                pending_manifest,
                RICH_GENERATION_POLICY_VERSION
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    interrupted.request_id = Uuid::new_v4();
    assert_eq!(
        knowledge
            .save_on_demand_manifest(
                &scope,
                build_on_demand_manifest(&scope, &interrupted, &release).unwrap()
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        replica
            .start_on_demand(&scope, request, manifest, "new-policy")
            .await
            .unwrap(),
        replica
            .get_execution(&scope, execution.execution_id)
            .await
            .unwrap()
            .unwrap()
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM content_executions WHERE project_id=$1")
            .bind(project.id.as_uuid())
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert_eq!(
        count, 1,
        "failed admission must roll back the execution row"
    );
}
