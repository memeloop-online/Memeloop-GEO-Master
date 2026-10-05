use std::sync::Arc;

use async_trait::async_trait;
use axum::{Json, Router, routing::get};
use chrono::{DateTime, Duration, Utc};
use geo_api::{
    AppState, BrowserBridge, ChannelService, ContentService, EventBus, MemoryIdempotencyStore,
    MemoryOperationStore, ModelProviderBridge, RepositoryHostOps,
    distribution::DistributionService,
};
use geo_domain::{
    AppError, ChannelAccount, ChannelAccountRecord, ChannelJobRepository, ChannelOutcome,
    ChannelOutcomeStatus, ChannelOwnerKind, ChannelRepository, ChannelStatus, ChannelTarget,
    ChannelTargetInput, ConnectorCapabilityRepository, ConnectorKey, ConnectorVerification,
    ContentItemStatus, ContentRepository, DistributionRepository, DistributionScope,
    DistributionScopeMode, DistributionTargetStatus, DocumentManifestPlanRequest, DocumentScope,
    ErrorCode, FreezeDistribution, ImportItem, InitialSource, InitialSourceKind,
    InitialSourceVisibility, IntentVerification, KnowledgePurpose, KnowledgeRepository,
    MemoryAuthRepository, MemoryChannelJobRepository, MemoryChannelRepository,
    MemoryConnectorCapabilityRepository, MemoryContentRepository, MemoryDistributionRepository,
    MemoryKnowledgeRepository, MemoryProjectRepository, PLAIN_TEXT_ARTICLE_FORMAT,
    PlatformPlacement, PreparedDistribution, ProjectCreate, ProjectPatch, ProjectRepository,
    ProjectSettings, ProjectStartCommand, PublicationLookupCandidate, PublicationLookupFinding,
    PublicationLookupJob, PublicationLookupObservation, PublicationLookupRepository, SourceKind,
    TenantScope, hash_idempotency_key, settings_hash, start_request_hash,
};
use geo_worker::{
    DistributionReadRequest, DistributionResumeRequest, DistributionStartRequest,
    DistributionTargetsReadRequest, HostOpError, HostOpErrorCode, HostOps, ModelCompletion,
    ModelCompletionRequest,
};
use uuid::Uuid;

struct ReadOnlyLookup {
    job: PublicationLookupJob,
    observation: PublicationLookupObservation,
    unavailable: bool,
}

#[async_trait]
impl PublicationLookupRepository for ReadOnlyLookup {
    async fn enqueue(
        &self,
        _: &TenantScope,
        _: Uuid,
        _: Uuid,
        _: DateTime<Utc>,
    ) -> Result<PublicationLookupJob, AppError> {
        panic!("read must not schedule a lookup")
    }
    async fn scan_due(
        &self,
        _: Option<Uuid>,
        _: DateTime<Utc>,
        _: usize,
    ) -> Result<Vec<PublicationLookupCandidate>, AppError> {
        panic!("read must not scan lookups")
    }
    async fn claim(
        &self,
        _: &TenantScope,
        _: Uuid,
        _: Uuid,
        _: DateTime<Utc>,
        _: DateTime<Utc>,
    ) -> Result<PublicationLookupJob, AppError> {
        panic!("read must not claim a lookup")
    }
    async fn finish(
        &self,
        _: &TenantScope,
        _: Uuid,
        _: PublicationLookupObservation,
        _: Option<DateTime<Utc>>,
    ) -> Result<PublicationLookupJob, AppError> {
        panic!("read must not mutate lookup observations")
    }
    async fn get(
        &self,
        _: &TenantScope,
        attempt_id: Uuid,
    ) -> Result<PublicationLookupJob, AppError> {
        assert_eq!(attempt_id, self.job.attempt_id);
        if self.unavailable {
            return Err(AppError::new(
                ErrorCode::DependencyUnavailable,
                "lookup read unavailable",
            ));
        }
        Ok(self.job.clone())
    }
    async fn observations(
        &self,
        _: &TenantScope,
        _: Uuid,
    ) -> Result<Vec<PublicationLookupObservation>, AppError> {
        panic!("read must not fetch unbounded lookup history")
    }
    async fn observation_page(
        &self,
        _: &TenantScope,
        attempt_id: Uuid,
        before: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<PublicationLookupObservation>, AppError> {
        assert_eq!(attempt_id, self.job.attempt_id);
        assert!(before.is_none());
        assert_eq!(limit, 20);
        Ok(vec![self.observation.clone()])
    }
}

struct GroundedModel;
#[async_trait]
impl ModelProviderBridge for GroundedModel {
    async fn complete(
        &self,
        _scope: &TenantScope,
        request: &ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError> {
        let input: serde_json::Value = serde_json::from_str(&request.prompt).unwrap();
        let citation = input["evidence"][0]["chunk_id"].as_str().unwrap();
        let text = if request
            .system
            .as_deref()
            .unwrap_or_default()
            .contains("checker")
        {
            let block = input["document"]["blocks"][0]["block_id"].as_str().unwrap();
            let title = input["title_check_id"].as_str().unwrap();
            serde_json::json!({"checks":[
                {"block_id":title,"verdict":"supported","citation_ids":[citation],
                 "detail":"Verified from the public source"},
                {"block_id":block,"verdict":"supported","citation_ids":[citation],
                 "detail":"Verified from the public source"}]})
            .to_string()
        } else {
            serde_json::json!({"title":"A documented answer","blocks":[{
                "kind":"paragraph","text":"Public description",
                "citation_ids":[citation],"items":[]}]})
            .to_string()
        };
        Ok(ModelCompletion {
            text,
            tool_calls: vec![],
            model: "injected".into(),
            prompt_tokens: 1,
            completion_tokens: 1,
            finish_reason: "stop".into(),
        })
    }
}

#[tokio::test]
async fn saved_article_proof_enables_frozen_semantic_coverage_without_mutating_old_freeze() {
    let fixture = setup(false).await;
    let registry = Arc::new(MemoryConnectorCapabilityRepository::default());
    let key = ConnectorKey {
        platform_id: "a".into(),
        placement_slot: "primary".into(),
    };
    let now = Utc::now();
    let url = "https://example.com/public".to_owned();
    let hash = "a".repeat(64);
    let published = ChannelOutcome {
        status: ChannelOutcomeStatus::Published,
        detail: None,
        occurred_at: now,
        raw_answer: None,
        citations: vec![],
        public_url: Some(url.clone()),
        screenshot_ref: None,
        connector_version: Some("live.v1".into()),
        runner_evidence: vec![],
        fixture: false,
    };
    registry
        .insert_verification(
            fixture.scope.operator_id,
            ConnectorVerification {
                verification_id: Uuid::new_v4(),
                key: key.clone(),
                connector_version: "live.v1".into(),
                content_type: PLAIN_TEXT_ARTICLE_FORMAT.into(),
                publication_receipt: published.clone(),
                public_readback: ChannelOutcome {
                    status: ChannelOutcomeStatus::Verified,
                    runner_evidence: vec![serde_json::json!({
                        "kind":"public_readback", "url":url,
                        "content_matched":true, "owned_by_account":true,
                        "expected_sha256":hash, "readback_sha256":hash
                    })],
                    ..published
                },
                verified_at: now,
            },
        )
        .await
        .unwrap();
    registry
        .configure(
            fixture.scope.operator_id,
            key.clone(),
            0,
            true,
            vec![PLAIN_TEXT_ARTICLE_FORMAT.into()],
            "live.v1",
        )
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let runner = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/v1/capabilities",
                get(|| async {
                    Json(serde_json::json!({"connectors":[{
                        "platform":"a", "placement_slot":"primary",
                        "connector_version":"live.v1", "operations":["publish"],
                        "verified":false
                    }]}))
                }),
            ),
        )
        .await
        .unwrap();
    });
    let service = DistributionService::new(
        fixture.distribution.clone(),
        fixture.content.clone(),
        fixture.knowledge.clone(),
        fixture.projects.clone(),
        fixture.channels.clone(),
    )
    .with_connector_registry(
        registry.clone(),
        Some(BrowserBridge::new(format!("http://{addr}"), "test-token".into()).unwrap()),
    );
    let frozen = service
        .freeze(&fixture.scope, fixture.cycle_id)
        .await
        .unwrap();
    let platform = frozen
        .platform_scope
        .iter()
        .find(|placement| placement.platform_id == "a")
        .unwrap();
    assert_eq!(
        platform.supported_formats,
        vec!["company_profile", "faq"],
        "wire-format proof must freeze document semantics, not the wire key"
    );
    assert_eq!(platform.unavailable_reason, None);
    registry
        .configure(fixture.scope.operator_id, key, 1, false, vec![], "")
        .await
        .unwrap();
    assert_eq!(
        service
            .freeze(&fixture.scope, fixture.cycle_id)
            .await
            .unwrap(),
        frozen,
        "explicit disable cannot rewrite an existing frozen manifest"
    );
    runner.abort();
}

fn placement(name: &str, formats: &[&str], unavailable: Option<&str>) -> PlatformPlacement {
    PlatformPlacement {
        platform_id: name.into(),
        placement_slot: "primary".into(),
        capability_version: "fixture-capabilities-v1".into(),
        supported_formats: formats.iter().map(|value| (*value).to_owned()).collect(),
        unavailable_reason: unavailable.map(str::to_owned),
        fixture: true,
    }
}

struct Fixture {
    scope: TenantScope,
    cycle_id: Uuid,
    projects: Arc<MemoryProjectRepository>,
    knowledge: Arc<MemoryKnowledgeRepository>,
    content: Arc<MemoryContentRepository>,
    channels: Arc<MemoryChannelRepository>,
    distribution: Arc<MemoryDistributionRepository>,
    service: DistributionService,
}

async fn setup(second_blocked: bool) -> Fixture {
    let tenant = TenantScope::new(Uuid::new_v4().into(), Uuid::new_v4().into(), None);
    let projects = Arc::new(MemoryProjectRepository::default());
    let project = projects
        .create(
            &tenant,
            ProjectCreate {
                slug: None,
                display_name: "Generic fixture".into(),
                settings: ProjectSettings {
                    brand_name: "Example".into(),
                    market: "US".into(),
                    language: "en".into(),
                    initial_sources: vec![InitialSource {
                        kind: InitialSourceKind::Text,
                        value: "Public description".into(),
                        visibility: InitialSourceVisibility::Public,
                        version_ref: None,
                        content_hash: None,
                    }],
                    document_scope: DocumentScope {
                        content_types: vec!["faq".into(), "company_profile".into()],
                        ..Default::default()
                    },
                    distribution_scope: DistributionScope {
                        mode: DistributionScopeMode::Explicit,
                        included_platform_ids: vec!["a".into(), "b".into(), "c".into()],
                        ..Default::default()
                    },
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap();
    let hash = settings_hash(&project.settings.clone().validate_start().unwrap()).unwrap();
    let started = projects
        .start(
            &tenant,
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("distribution-fixture-start"),
                request_hash: start_request_hash(project.id, project.revision, &hash),
                settings_hash: hash,
                operation_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(tenant.operator_id, tenant.tenant_id, Some(project.id));
    let knowledge = Arc::new(MemoryKnowledgeRepository::default());
    let imported = knowledge
        .import_batch(
            &scope,
            vec![ImportItem {
                client_item_id: "public".into(),
                kind: SourceKind::Text,
                name: "generic public input".into(),
                purpose: KnowledgePurpose::Public,
                text: Some("Public description".into()),
                url: None,
                object_id: None,
                knowledge_release_id: None,
            }],
        )
        .await
        .unwrap();
    let release_id = imported.items[0]
        .release
        .as_ref()
        .unwrap()
        .knowledge_release_id;
    let planned = knowledge
        .plan_document_manifest(
            &scope,
            DocumentManifestPlanRequest {
                manifest_id: started.document_manifest.manifest_id,
                knowledge_release_id: release_id,
            },
            project.settings.document_scope,
        )
        .await
        .unwrap();
    assert_eq!(planned.items.len(), 2);
    let content = Arc::new(MemoryContentRepository::default());
    let first_stage = ContentService::new(content.clone(), knowledge.clone(), projects.clone())
        .with_model_provider(Arc::new(GroundedModel));
    let execution = first_stage.start(&scope, started.cycle_id).await.unwrap();
    let items = content
        .list_items(&scope, execution.execution_id)
        .await
        .unwrap();
    for (index, item) in items.iter().enumerate() {
        if index == 1 && second_blocked {
            content
                .classify(
                    &scope,
                    execution.execution_id,
                    item.item_id,
                    ContentItemStatus::Blocked,
                    "fact_conflict",
                )
                .await
                .unwrap();
            continue;
        }
        first_stage
            .prepare(&scope, execution.execution_id, item.item_id)
            .await
            .unwrap();
        first_stage
            .generate(&scope, execution.execution_id, item.item_id)
            .await
            .unwrap();
        assert_eq!(
            first_stage
                .check(&scope, execution.execution_id, item.item_id)
                .await
                .unwrap()
                .status,
            ContentItemStatus::Ready
        );
    }
    first_stage
        .close(&scope, execution.execution_id)
        .await
        .unwrap();
    let channels = Arc::new(MemoryChannelRepository::default());
    let distribution = Arc::new(MemoryDistributionRepository::new());
    let service = DistributionService::new(
        distribution.clone(),
        content.clone(),
        knowledge.clone(),
        projects.clone(),
        channels.clone(),
    )
    .with_capability_snapshot(vec![
        placement("a", &["faq", "company_profile"], None),
        placement("b", &["faq", "company_profile"], Some("runner_unavailable")),
        placement("c", &["unknown_format"], None),
    ]);
    Fixture {
        scope,
        cycle_id: started.cycle_id,
        projects,
        knowledge,
        content,
        channels,
        distribution,
        service,
    }
}

async fn add_account(fixture: &Fixture) {
    let now = Utc::now();
    fixture
        .channels
        .save_account(
            &fixture.scope,
            ChannelAccountRecord {
                account: ChannelAccount {
                    account_id: Uuid::new_v4(),
                    project_id: fixture.scope.project_id.unwrap(),
                    owner_kind: ChannelOwnerKind::Customer,
                    platform: "a".into(),
                    group_id: None,
                    status: ChannelStatus::Ready,
                    display_name: Some("generic account".into()),
                    platform_account_id: Some("fixture-identity".into()),
                    avatar_url: None,
                    enabled: true,
                    proxy_configured: false,
                    proxy_server: None,
                    created_at: now,
                    updated_at: now,
                },
                session: None,
                proxy: None,
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn publication_target_resolves_original_across_cycles_without_mutation_or_scope_leak() {
    let fixture = setup(true).await;
    add_account(&fixture).await;
    let first = fixture
        .service
        .freeze(&fixture.scope, fixture.cycle_id)
        .await
        .unwrap();
    fixture
        .service
        .resume(&fixture.scope, first.manifest_id, 1)
        .await
        .unwrap();
    let rows = fixture
        .service
        .targets(&fixture.scope, first.manifest_id, None, 10)
        .await
        .unwrap()
        .rows;
    let sent = rows
        .iter()
        .find(|row| row.status == DistributionTargetStatus::Ready)
        .unwrap();
    let binding = fixture
        .service
        .publication_target(&fixture.scope, first.manifest_id, sent.target_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(binding.distribution_target_id, sent.target_id);
    assert_eq!(binding.channel_target_id, sent.target_id);
    assert_eq!(
        binding.publication_intent_id,
        sent.publication_intent_id.unwrap()
    );
    let unbound = rows
        .iter()
        .find(|row| row.publication_intent_id.is_none())
        .unwrap();
    assert_eq!(
        fixture
            .service
            .publication_target(&fixture.scope, first.manifest_id, unbound.target_id)
            .await
            .unwrap(),
        None
    );
    fixture
        .distribution
        .record_intent_verification(
            &fixture.scope,
            binding.publication_intent_id,
            IntentVerification::Unknown,
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    let revision = fixture
        .distribution
        .get_publication_bundle(&fixture.scope, binding.publication_intent_id)
        .await
        .unwrap()
        .revision;
    let execution = fixture
        .content
        .list_executions(&fixture.scope, fixture.cycle_id)
        .await
        .unwrap()
        .remove(0);
    let handoff = fixture
        .content
        .get_handoff(&fixture.scope, execution.execution_id)
        .await
        .unwrap()
        .unwrap();
    let documents = fixture
        .knowledge
        .get_document_manifest(&fixture.scope, first.document_manifest_id)
        .await
        .unwrap()
        .unwrap();
    let cutoff = fixture
        .projects
        .get_report_cycle(
            &fixture.scope,
            fixture.scope.project_id.unwrap(),
            fixture.cycle_id,
        )
        .await
        .unwrap()
        .unwrap()
        .cutoff_at;
    let later_cycle = fixture
        .projects
        .schedule_next_cycle(
            &fixture.scope,
            fixture.scope.project_id.unwrap(),
            fixture.cycle_id,
            cutoff + Duration::seconds(1),
        )
        .await
        .unwrap()
        .cycle_id;
    let second = fixture
        .distribution
        .freeze(
            &fixture.scope,
            FreezeDistribution {
                cycle_id: later_cycle,
                content_execution: geo_domain::ContentExecution {
                    cycle_id: later_cycle,
                    ..execution
                },
                document_manifest: documents,
                content_handoff: handoff,
                placements: first.platform_scope.clone(),
                revision: 1,
                sealed_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    let page = fixture
        .distribution
        .expansion_page(&fixture.scope, second.manifest_id, 0, 64)
        .await
        .unwrap();
    fixture
        .distribution
        .commit_expansion_page(&fixture.scope, second.manifest_id, 0, page.rows)
        .await
        .unwrap();
    let later_row = fixture
        .distribution
        .list_targets(&fixture.scope, second.manifest_id, None, 10)
        .await
        .unwrap()
        .rows
        .into_iter()
        .find(|row| {
            row.document_item_id == sent.document_item_id && row.platform_id == sent.platform_id
        })
        .unwrap();
    let reused = fixture
        .distribution
        .materialize(
            &fixture.scope,
            PreparedDistribution {
                manifest_id: second.manifest_id,
                target_id: later_row.target_id,
                revision: Some(revision),
                account_id: sent.account_id,
                defer_reason: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        reused.target.status,
        DistributionTargetStatus::ReusedUnknown
    );
    assert_ne!(reused.target.target_id, sent.target_id);
    let resolved = fixture
        .service
        .publication_target(&fixture.scope, second.manifest_id, reused.target.target_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.distribution_target_id, reused.target.target_id);
    assert_eq!(resolved.channel_target_id, sent.target_id);
    assert_eq!(
        resolved.publication_intent_id,
        binding.publication_intent_id
    );
    let state = AppState::with_stores_and_auth_and_projects_and_knowledge(
        Arc::new(MemoryOperationStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
        Arc::new(MemoryAuthRepository::development_with_password("unused")),
        fixture.projects.clone(),
        fixture.knowledge.clone(),
        EventBus::default(),
        false,
    )
    .with_content_repository(fixture.content.clone())
    .with_distribution_repository(fixture.distribution.clone())
    .with_channel_service(ChannelService::unconfigured(fixture.channels.clone()));
    let empty = RepositoryHostOps::new(fixture.knowledge.clone()).with_content(state.clone());
    let request = DistributionTargetsReadRequest {
        manifest_id: second.manifest_id,
        after_ordinal: None,
        limit: Some(10),
    };
    let page = empty
        .distribution_targets_read(&fixture.scope, request.clone())
        .await
        .unwrap();
    let item = page
        .items
        .iter()
        .find(|item| item.target_id == reused.target.target_id)
        .unwrap();
    assert_eq!(
        item.original_channel_target_id,
        Some(binding.channel_target_id)
    );
    assert!(
        item.publication_lookup.is_none(),
        "outbox projection has not created the original channel job"
    );
    assert!(
        page.items
            .iter()
            .any(|item| item.publication_intent_id.is_none()
                && item.original_channel_target_id.is_none())
    );

    let bundle = fixture
        .distribution
        .get_publication_bundle(&fixture.scope, binding.publication_intent_id)
        .await
        .unwrap();
    let account_id = sent.account_id.unwrap();
    let generated = ChannelTargetInput::GeneratedPublish {
        content_revision_id: bundle.revision.revision_id,
        variant_id: bundle.variant.variant_id,
        publication_intent_id: bundle.intent.intent_id,
        distribution_target_id: sent.target_id,
        platform: sent.platform_id.clone(),
        account_id,
        title: bundle.variant.title.clone(),
        body_sha256: geo_domain::sha256_hex(bundle.variant.markdown.as_bytes()),
        body: bundle.variant.markdown.clone(),
        payload_hash: bundle.variant.payload_hash.clone(),
        evidence: bundle.revision.evidence.clone(),
    };
    let original_jobs = Arc::new(MemoryChannelJobRepository::default());
    original_jobs
        .insert_generated_target(
            &fixture.scope,
            fixture.cycle_id,
            binding.channel_target_id,
            ChannelTarget {
                target_id: binding.channel_target_id,
                input: generated.clone(),
            },
        )
        .await
        .unwrap();
    let (_, attempt) = original_jobs
        .claim(
            &fixture.scope,
            binding.channel_target_id,
            Uuid::new_v4(),
            Utc::now(),
        )
        .await
        .unwrap();
    let at = Utc::now();
    original_jobs
        .finish(
            &fixture.scope,
            binding.channel_target_id,
            attempt.attempt_id,
            ChannelOutcome {
                status: ChannelOutcomeStatus::Unknown,
                detail: Some("private opaque evidence".into()),
                occurred_at: at,
                raw_answer: None,
                citations: vec![],
                public_url: Some("https://example.invalid/private-asset".into()),
                screenshot_ref: None,
                connector_version: None,
                runner_evidence: vec![serde_json::json!({"private":"opaque"})],
                fixture: false,
            },
            at,
        )
        .await
        .unwrap();
    let job = PublicationLookupJob {
        attempt_id: attempt.attempt_id,
        target_id: binding.channel_target_id,
        account_id,
        frozen_input: generated,
        connector_version: None,
        candidate_public_url: Some("https://example.invalid/private-asset".into()),
        next_due_at: Some(at + Duration::minutes(5)),
        lease_execution_id: None,
        lease_expires_at: None,
        query_count: 1,
        last_error_code: Some("opaque connector error".into()),
    };
    let observation = PublicationLookupObservation {
        execution_id: Uuid::new_v4(),
        attempt_id: attempt.attempt_id,
        finding: PublicationLookupFinding::AssetObserved,
        evidence: serde_json::json!({"public_url":"https://example.invalid/private-asset", "raw":"opaque"}),
        observed_at: at,
        received_at: at + Duration::seconds(1),
        error_code: None,
    };
    let state = state
        .with_channel_job_repository(original_jobs.clone())
        .with_publication_lookup_repository(Arc::new(ReadOnlyLookup {
            job: job.clone(),
            observation: observation.clone(),
            unavailable: false,
        }));
    let with_lookup = RepositoryHostOps::new(fixture.knowledge.clone()).with_content(state.clone());
    let page = with_lookup
        .distribution_targets_read(&fixture.scope, request.clone())
        .await
        .unwrap();
    let item = page
        .items
        .iter()
        .find(|item| item.target_id == reused.target.target_id)
        .unwrap();
    assert_eq!(item.status, DistributionTargetStatus::ReusedUnknown);
    assert_eq!(
        item.original_channel_target_id,
        Some(binding.channel_target_id)
    );
    let lookup = item.publication_lookup.as_ref().unwrap();
    assert_eq!(lookup.query_count, 1);
    assert_eq!(lookup.last_error_code.as_deref(), Some("lookup_error"));
    assert_eq!(
        lookup.latest_observation.as_ref().unwrap().finding,
        PublicationLookupFinding::AssetObserved
    );
    let serialized = serde_json::to_string(item).unwrap();
    for forbidden in [
        "private-asset",
        "opaque",
        "raw",
        "account_id",
        "candidate_public_url",
    ] {
        assert!(!serialized.contains(forbidden), "leaked {forbidden}");
    }
    assert_eq!(
        original_jobs
            .get_target(&fixture.scope, binding.channel_target_id)
            .await
            .unwrap()
            .attempts
            .len(),
        1
    );
    assert_eq!(
        fixture
            .distribution
            .publication_commands(&fixture.scope)
            .await
            .len(),
        1
    );
    let failed = state.with_publication_lookup_repository(Arc::new(ReadOnlyLookup {
        job,
        observation,
        unavailable: true,
    }));
    let error = RepositoryHostOps::new(fixture.knowledge.clone())
        .with_content(failed)
        .distribution_targets_read(&fixture.scope, request)
        .await
        .unwrap_err();
    assert_eq!(
        error.code,
        HostOpErrorCode::Failed,
        "lookup failures must not fabricate a clean state"
    );
    assert_eq!(
        fixture
            .distribution
            .publication_commands(&fixture.scope)
            .await
            .len(),
        1
    );
    assert_eq!(
        fixture
            .service
            .target(&fixture.scope, second.manifest_id, reused.target.target_id)
            .await
            .unwrap(),
        reused.target
    );
    for foreign in [
        TenantScope::new(
            Uuid::new_v4().into(),
            fixture.scope.tenant_id,
            fixture.scope.project_id,
        ),
        TenantScope::new(
            fixture.scope.operator_id,
            Uuid::new_v4().into(),
            fixture.scope.project_id,
        ),
        TenantScope::new(
            fixture.scope.operator_id,
            fixture.scope.tenant_id,
            Some(Uuid::new_v4().into()),
        ),
    ] {
        assert_eq!(
            with_lookup
                .distribution_targets_read(
                    &foreign,
                    DistributionTargetsReadRequest {
                        manifest_id: second.manifest_id,
                        after_ordinal: None,
                        limit: Some(10),
                    },
                )
                .await
                .unwrap_err()
                .code,
            HostOpErrorCode::NotFound
        );
        assert_eq!(
            fixture
                .service
                .publication_target(&foreign, second.manifest_id, reused.target.target_id)
                .await
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
    }
    assert_eq!(
        fixture
            .service
            .publication_target(&fixture.scope, first.manifest_id, reused.target.target_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn agent_distribution_tools_use_service_with_current_cycle_and_reference_only_pages() {
    let fixture = setup(true).await;
    add_account(&fixture).await;
    let state = AppState::with_stores_and_auth_and_projects_and_knowledge(
        Arc::new(MemoryOperationStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
        Arc::new(MemoryAuthRepository::development_with_password("unused")),
        fixture.projects.clone(),
        fixture.knowledge.clone(),
        EventBus::default(),
        false,
    )
    .with_content_repository(fixture.content.clone())
    .with_distribution_repository(fixture.distribution.clone())
    .with_channel_service(ChannelService::unconfigured(fixture.channels.clone()));
    let host = RepositoryHostOps::new(fixture.knowledge.clone()).with_content(state);
    let other_project = TenantScope::new(
        fixture.scope.operator_id,
        fixture.scope.tenant_id,
        Some(Uuid::new_v4().into()),
    );
    assert_eq!(
        host.distribution_start(&other_project, DistributionStartRequest { cycle_id: None })
            .await
            .unwrap_err()
            .code,
        HostOpErrorCode::NotFound
    );
    assert_eq!(
        host.distribution_read(
            &fixture.scope,
            DistributionReadRequest {
                cycle_id: None,
                manifest_id: None,
            }
        )
        .await
        .unwrap_err()
        .code,
        HostOpErrorCode::NotFound
    );
    let started = host
        .distribution_start(&fixture.scope, DistributionStartRequest { cycle_id: None })
        .await
        .unwrap();
    assert_eq!(started.cycle_id, fixture.cycle_id);
    assert_eq!(started.expected_count, 6);
    assert!(started.complete);
    assert_eq!(started.expansion_cursor, 6);
    assert_eq!(
        started,
        host.distribution_start(
            &fixture.scope,
            DistributionStartRequest {
                cycle_id: Some(fixture.cycle_id)
            }
        )
        .await
        .unwrap()
    );
    assert_eq!(
        started,
        host.distribution_read(
            &fixture.scope,
            DistributionReadRequest {
                cycle_id: None,
                manifest_id: None,
            }
        )
        .await
        .unwrap()
    );
    let first = host
        .distribution_targets_read(
            &fixture.scope,
            DistributionTargetsReadRequest {
                manifest_id: started.manifest_id,
                after_ordinal: None,
                limit: Some(2),
            },
        )
        .await
        .unwrap();
    assert_eq!(first.expected_count, 6);
    assert_eq!(first.items.len(), 2);
    assert!(first.next_ordinal.is_some());
    let second = host
        .distribution_targets_read(
            &fixture.scope,
            DistributionTargetsReadRequest {
                manifest_id: started.manifest_id,
                after_ordinal: first.next_ordinal,
                limit: Some(2),
            },
        )
        .await
        .unwrap();
    assert_eq!(second.items.len(), 2);
    assert!(second.items[0].ordinal > first.items[1].ordinal);
    let view = serde_json::to_value(&first).unwrap();
    let serialized = view.to_string();
    for forbidden in [
        "account_id",
        "markdown",
        "payload_hash",
        "supported_formats",
        "capability_version",
        "platform_account_id",
    ] {
        assert!(!serialized.contains(forbidden), "leaked {forbidden}");
    }
    assert_eq!(
        host.distribution_resume(
            &fixture.scope,
            DistributionResumeRequest {
                manifest_id: started.manifest_id,
                after_ordinal: None,
            }
        )
        .await
        .unwrap(),
        started
    );
    assert_eq!(
        host.distribution_targets_read(
            &other_project,
            DistributionTargetsReadRequest {
                manifest_id: started.manifest_id,
                after_ordinal: None,
                limit: None,
            }
        )
        .await
        .unwrap_err()
        .code,
        HostOpErrorCode::NotFound
    );
    // In-memory channel count and IDs cannot claim real connector support.
    // The unverified server snapshot leaves all targets non-publishable.
    assert!(
        first
            .items
            .iter()
            .all(|item| item.status != DistributionTargetStatus::Ready)
    );
}

#[tokio::test]
async fn two_documents_three_platforms_preserve_all_cells_and_only_queue_checked_revisions() {
    let fixture = setup(true).await;
    add_account(&fixture).await;
    let frozen = fixture
        .service
        .freeze(&fixture.scope, fixture.cycle_id)
        .await
        .unwrap();
    assert_eq!(frozen.expected_count, 6);
    assert_eq!(frozen.expansion_cursor, 0);
    let result = fixture
        .service
        .resume(&fixture.scope, frozen.manifest_id, 1)
        .await
        .unwrap();
    assert!(result.complete);
    let page = fixture
        .service
        .targets(&fixture.scope, frozen.manifest_id, None, 10)
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 6);
    assert_eq!(
        page.rows
            .iter()
            .filter(|target| target.status == DistributionTargetStatus::Ready)
            .count(),
        1
    );
    assert_eq!(
        page.rows
            .iter()
            .filter(|target| target.status == DistributionTargetStatus::Deferred)
            .count(),
        1
    );
    assert_eq!(
        page.rows
            .iter()
            .filter(|target| target.status == DistributionTargetStatus::NotApplicable)
            .count(),
        1
    );
    assert_eq!(
        page.rows
            .iter()
            .filter(|target| target.status == DistributionTargetStatus::Blocked)
            .count(),
        3
    );
    let first = &page.rows[0];
    assert!(first.variant_id.is_some());
    assert!(first.publication_intent_id.is_some());
    assert_eq!(
        fixture
            .service
            .target(&fixture.scope, frozen.manifest_id, first.target_id)
            .await
            .unwrap(),
        *first
    );
    let commands = fixture
        .distribution
        .publication_commands(&fixture.scope)
        .await;
    assert_eq!(
        commands.len(),
        1,
        "outbox means intent, not a successful publication"
    );
    assert!(
        commands[0].fixture,
        "fixture connector must stay marked simulated"
    );
    fixture
        .service
        .resume(&fixture.scope, frozen.manifest_id, 1)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .distribution
            .publication_commands(&fixture.scope)
            .await
            .len(),
        1
    );
    let foreign = TenantScope::new(
        Uuid::new_v4().into(),
        fixture.scope.tenant_id,
        fixture.scope.project_id,
    );
    assert_eq!(
        fixture
            .service
            .get(&foreign, frozen.manifest_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn frozen_scope_does_not_drift_and_delayed_account_uses_sealed_checked_revision() {
    let fixture = setup(false).await;
    let frozen = fixture
        .service
        .freeze(&fixture.scope, fixture.cycle_id)
        .await
        .unwrap();
    let first = fixture
        .service
        .resume(&fixture.scope, frozen.manifest_id, 1)
        .await
        .unwrap();
    assert!(first.complete);
    assert_eq!(first.expected_count, 6);
    let before = fixture
        .service
        .targets(&fixture.scope, frozen.manifest_id, None, 10)
        .await
        .unwrap();
    assert_eq!(before.rows[0].reason.as_deref(), Some("account_unassigned"));
    let project_id = fixture.scope.project_id.unwrap();
    let current = fixture
        .projects
        .get(&fixture.scope, project_id)
        .await
        .unwrap()
        .unwrap();
    fixture
        .projects
        .update(
            &fixture.scope,
            project_id,
            current.revision,
            ProjectPatch {
                distribution_scope: Some(DistributionScope {
                    mode: DistributionScopeMode::Explicit,
                    included_platform_ids: vec!["different".into()],
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    // A post-freeze edit creates a new current draft but must not mutate the
    // immutable checked revision or the earlier distribution handoff.
    let source_row = &before.rows[0];
    let execution = fixture
        .content
        .list_executions(&fixture.scope, fixture.cycle_id)
        .await
        .unwrap()
        .remove(0);
    let item = fixture
        .content
        .get_item(
            &fixture.scope,
            execution.execution_id,
            source_row.document_item_id,
        )
        .await
        .unwrap()
        .unwrap();
    let old_revision = fixture
        .content
        .list_revisions(&fixture.scope, item.asset_id.unwrap())
        .await
        .unwrap()
        .remove(0);
    let mut edited = old_revision.document.clone();
    edited.title = "Later draft awaiting check".into();
    fixture
        .content
        .edit(
            &fixture.scope,
            item.asset_id.unwrap(),
            old_revision.revision_id,
            edited,
        )
        .await
        .unwrap();
    add_account(&fixture).await;
    let replay = fixture
        .service
        .freeze(&fixture.scope, fixture.cycle_id)
        .await
        .unwrap();
    assert_eq!(replay.input_hash, frozen.input_hash);
    assert_eq!(replay.platform_scope, frozen.platform_scope);
    fixture
        .service
        .resume(&fixture.scope, frozen.manifest_id, 1)
        .await
        .unwrap();
    let after = fixture
        .service
        .targets(&fixture.scope, frozen.manifest_id, None, 10)
        .await
        .unwrap();
    assert_eq!(
        after.rows[0].content_revision_id,
        Some(old_revision.revision_id)
    );
    assert_eq!(after.rows[0].status, DistributionTargetStatus::Ready);
    assert_eq!(
        fixture
            .distribution
            .publication_commands(&fixture.scope)
            .await
            .len(),
        2
    );
}
