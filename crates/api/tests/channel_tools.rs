use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use geo_api::{AppState, RepositoryHostOps};
use geo_domain::{
    ChannelAccount, ChannelAccountRecord, ChannelOutcome, ChannelOutcomeStatus, ChannelOwnerKind,
    ChannelStatus, CreateQuestionSet, DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, ImportItem,
    KnowledgePurpose, ProjectCreate, ProjectSettings, ProjectStartCommand,
    PublicationLookupCandidate, PublicationLookupFinding, PublicationLookupJob,
    PublicationLookupObservation, PublicationLookupRepository, QuestionDraft, QuestionSource,
    QuestionSourceKind, SourceKind, TenantScope, hash_idempotency_key, settings_hash,
    start_request_hash,
};
use geo_worker::{
    ChannelBoundMeasurementPlanItem, ChannelDiscoverRequest, ChannelDiscoveryItem,
    ChannelDiscoveryKind, ChannelExecutionState, ChannelManifestReadRequest, ChannelPlanRequest,
    ChannelPublicationPlanItem, ChannelTargetExecuteRequest, HostOpErrorCode, HostOps,
    QuestionDiscoverRequest, QuestionReviseRequest,
};
use uuid::Uuid;

struct ReadOnlyLookup {
    job: PublicationLookupJob,
    latest: PublicationLookupObservation,
    fail_read: bool,
}

#[async_trait]
impl PublicationLookupRepository for ReadOnlyLookup {
    async fn enqueue(
        &self,
        _: &TenantScope,
        _: Uuid,
        _: Uuid,
        _: DateTime<Utc>,
    ) -> Result<PublicationLookupJob, geo_domain::AppError> {
        panic!("manifest must never enqueue a publication lookup")
    }

    async fn scan_due(
        &self,
        _: Option<Uuid>,
        _: DateTime<Utc>,
        _: usize,
    ) -> Result<Vec<PublicationLookupCandidate>, geo_domain::AppError> {
        panic!("manifest must never scan lookup jobs")
    }

    async fn claim(
        &self,
        _: &TenantScope,
        _: Uuid,
        _: Uuid,
        _: DateTime<Utc>,
        _: DateTime<Utc>,
    ) -> Result<PublicationLookupJob, geo_domain::AppError> {
        panic!("manifest must never claim a lookup job")
    }

    async fn finish(
        &self,
        _: &TenantScope,
        _: Uuid,
        _: PublicationLookupObservation,
        _: Option<DateTime<Utc>>,
    ) -> Result<PublicationLookupJob, geo_domain::AppError> {
        panic!("manifest must never write a lookup observation")
    }

    async fn get(
        &self,
        _: &TenantScope,
        attempt_id: Uuid,
    ) -> Result<PublicationLookupJob, geo_domain::AppError> {
        assert_eq!(attempt_id, self.job.attempt_id);
        if self.fail_read {
            return Err(geo_domain::AppError::new(
                geo_domain::ErrorCode::DependencyUnavailable,
                "lookup read unavailable",
            ));
        }
        Ok(self.job.clone())
    }

    async fn observations(
        &self,
        _: &TenantScope,
        _: Uuid,
    ) -> Result<Vec<PublicationLookupObservation>, geo_domain::AppError> {
        panic!("manifest must not read unbounded observation history")
    }

    async fn observation_page(
        &self,
        _: &TenantScope,
        attempt_id: Uuid,
        before: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<PublicationLookupObservation>, geo_domain::AppError> {
        assert_eq!(attempt_id, self.job.attempt_id);
        assert!(before.is_none());
        assert_eq!(limit, 20);
        Ok(vec![self.latest.clone()])
    }
}

struct Fixture {
    state: AppState,
    scope: TenantScope,
    cycle_id: Uuid,
    public_source: (Uuid, Uuid),
    internal_source: Uuid,
    account_ids: [Uuid; 2],
}

#[tokio::test]
async fn manifest_reports_lookup_observation_without_upgrading_or_resending_unknown_result() {
    let f = fixture().await;
    let receipt = ops(&f).channel_plan(&f.scope, plan(&f)).await.unwrap();
    let target_id = f
        .state
        .channel_job_repository()
        .get_plan(&f.scope, receipt.cycle_id)
        .await
        .unwrap()
        .unwrap()
        .targets[0]
        .target_id;
    let repository = f.state.channel_job_repository();
    let (view, attempt) = repository
        .claim(&f.scope, target_id, Uuid::new_v4(), Utc::now())
        .await
        .unwrap();
    let at = Utc::now();
    repository
        .finish(
            &f.scope,
            target_id,
            attempt.attempt_id,
            ChannelOutcome {
                status: ChannelOutcomeStatus::Unknown,
                detail: Some("opaque runner evidence".into()),
                occurred_at: at,
                raw_answer: None,
                citations: vec![],
                public_url: Some("https://example.invalid/private-asset".into()),
                screenshot_ref: None,
                connector_version: None,
                runner_evidence: vec![serde_json::json!({"opaque":"runner data"})],
                fixture: false,
            },
            at,
        )
        .await
        .unwrap();
    let job = PublicationLookupJob {
        attempt_id: attempt.attempt_id,
        target_id,
        account_id: view.input.account_id(),
        frozen_input: view.input.clone(),
        connector_version: None,
        candidate_public_url: None,
        next_due_at: Some(at + Duration::minutes(3)),
        lease_execution_id: None,
        lease_expires_at: Some(at + Duration::minutes(1)),
        query_count: 1,
        last_error_code: Some("opaque internal diagnostic".into()),
    };
    let latest = PublicationLookupObservation {
        execution_id: Uuid::new_v4(),
        attempt_id: attempt.attempt_id,
        finding: PublicationLookupFinding::AssetObserved,
        evidence: serde_json::json!({"public_url":"https://example.invalid/private-asset","raw_evidence":"opaque runner evidence"}),
        observed_at: at,
        received_at: at + Duration::seconds(1),
        error_code: None,
    };
    let state = f
        .state
        .clone()
        .with_publication_lookup_repository(Arc::new(ReadOnlyLookup {
            job: job.clone(),
            latest: latest.clone(),
            fail_read: false,
        }));
    let ops = RepositoryHostOps::new(state.knowledge_repository()).with_channels(state.clone());
    let manifest = ops
        .channel_manifest_read(
            &f.scope,
            ChannelManifestReadRequest {
                cycle_id: None,
                revision: None,
                cursor: None,
                limit: None,
            },
        )
        .await
        .unwrap();
    assert!(
        manifest
            .items
            .iter()
            .any(|item| { item.target_id != target_id && item.publication_lookup.is_none() })
    );
    let target = manifest
        .items
        .iter()
        .find(|item| item.target_id == target_id)
        .unwrap();
    assert_eq!(target.execution.state, ChannelExecutionState::UnknownResult);
    assert_eq!(target.execution.outcome_status.as_deref(), Some("unknown"));
    let lookup = target.publication_lookup.as_ref().unwrap();
    assert_eq!(lookup.query_count, 1);
    assert_eq!(lookup.last_error_code.as_deref(), Some("lookup_error"));
    assert_eq!(
        lookup.latest_observation.as_ref().unwrap().finding,
        PublicationLookupFinding::AssetObserved
    );
    assert_eq!(lookup.latest_observation.as_ref().unwrap().observed_at, at);
    let serialized = serde_json::to_string(&manifest).unwrap();
    for forbidden in [
        "private-asset",
        "opaque runner",
        "raw_evidence",
        "candidate_public_url",
        "account_id\":\"private",
    ] {
        assert!(!serialized.contains(forbidden), "lookup leaked evidence");
    }
    let replay = ops
        .channel_target_execute(&f.scope, ChannelTargetExecuteRequest { target_id })
        .await
        .unwrap();
    assert_eq!(replay.state, ChannelExecutionState::UnknownResult);
    assert_eq!(
        repository
            .get_target(&f.scope, target_id)
            .await
            .unwrap()
            .attempts
            .len(),
        1
    );
    let unavailable = state.with_publication_lookup_repository(Arc::new(ReadOnlyLookup {
        job,
        latest,
        fail_read: true,
    }));
    let unavailable_ops =
        RepositoryHostOps::new(unavailable.knowledge_repository()).with_channels(unavailable);
    assert!(
        unavailable_ops
            .channel_manifest_read(
                &f.scope,
                ChannelManifestReadRequest {
                    cycle_id: None,
                    revision: None,
                    cursor: None,
                    limit: None,
                },
            )
            .await
            .is_err()
    );
}

async fn fixture() -> Fixture {
    let state = AppState::development_with_password("local-fixture");
    let base = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let projects = state.project_repository();
    let project = projects
        .create(
            &base,
            ProjectCreate {
                slug: None,
                display_name: "Example project".into(),
                settings: ProjectSettings {
                    brand_name: "Example".into(),
                    market: "CN".into(),
                    language: "en".into(),
                    initial_sources: vec![geo_domain::InitialSource {
                        kind: geo_domain::InitialSourceKind::Text,
                        value: "Public material".into(),
                        visibility: geo_domain::InitialSourceVisibility::Public,
                        version_ref: None,
                        content_hash: None,
                    }],
                    ..ProjectSettings::default()
                },
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(base.operator_id, base.tenant_id, Some(project.id));
    let settings_hash = settings_hash(&project.settings).unwrap();
    let start = projects
        .start(
            &base,
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("channel-tool-fixture"),
                request_hash: start_request_hash(project.id, project.revision, &settings_hash),
                settings_hash,
                operation_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    let imports = state
        .knowledge_repository()
        .import_batch(
            &scope,
            vec![
                ImportItem {
                    client_item_id: "public-fixture".into(),
                    kind: SourceKind::Text,
                    name: "Approved source".into(),
                    purpose: KnowledgePurpose::Public,
                    text: Some("Approved fixture publication content.".into()),
                    url: None,
                    object_id: None,
                    knowledge_release_id: None,
                },
                ImportItem {
                    client_item_id: "internal-fixture".into(),
                    kind: SourceKind::Text,
                    name: "Internal material".into(),
                    purpose: KnowledgePurpose::Internal,
                    text: Some("Private fixture content must stay out of discovery.".into()),
                    url: None,
                    object_id: None,
                    knowledge_release_id: None,
                },
            ],
        )
        .await
        .unwrap();
    let public_source = (
        imports.items[0].source.as_ref().unwrap().source_id,
        imports.items[0]
            .source_version
            .as_ref()
            .unwrap()
            .source_version_id,
    );
    let internal_source = imports.items[1].source.as_ref().unwrap().source_id;
    let mut account_ids = [Uuid::nil(); 2];
    for account_id in &mut account_ids {
        *account_id = Uuid::new_v4();
        state
            .channel_service()
            .repository
            .save_account(
                &scope,
                ChannelAccountRecord {
                    account: ChannelAccount {
                        account_id: *account_id,
                        project_id: project.id,
                        owner_kind: ChannelOwnerKind::Customer,
                        platform: "zhihu".into(),
                        group_id: None,
                        status: ChannelStatus::NeedsLogin,
                        display_name: Some("Fixture account".into()),
                        platform_account_id: Some(format!("private-identity-{account_id}")),
                        avatar_url: None,
                        enabled: true,
                        proxy_configured: true,
                        proxy_server: Some("private-proxy-fixture".into()),
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                    },
                    session: None,
                    proxy: None,
                },
            )
            .await
            .unwrap();
    }
    Fixture {
        state,
        scope,
        cycle_id: start.cycle_id,
        public_source,
        internal_source,
        account_ids,
    }
}

fn ops(f: &Fixture) -> RepositoryHostOps {
    RepositoryHostOps::new(f.state.knowledge_repository()).with_channels(f.state.clone())
}

fn plan(f: &Fixture) -> ChannelPlanRequest {
    ChannelPlanRequest {
        cycle_id: None,
        publications: f
            .account_ids
            .iter()
            .map(|account_id| ChannelPublicationPlanItem {
                source_id: f.public_source.0,
                source_version_id: f.public_source.1,
                platform: "zhihu".into(),
                account_id: *account_id,
            })
            .collect(),
        measurements: vec![],
        bound_measurements: vec![],
    }
}

#[tokio::test]
async fn questions_are_scoped_and_heldout_discovery_is_reference_only() {
    let f = fixture().await;
    let ops = ops(&f);
    let measure_account_id = Uuid::new_v4();
    f.state
        .channel_service()
        .repository
        .save_account(
            &f.scope,
            ChannelAccountRecord {
                account: ChannelAccount {
                    account_id: measure_account_id,
                    project_id: f.scope.project_id.unwrap(),
                    owner_kind: ChannelOwnerKind::Customer,
                    platform: "kimi".into(),
                    group_id: None,
                    status: ChannelStatus::NeedsLogin,
                    display_name: None,
                    platform_account_id: None,
                    avatar_url: None,
                    enabled: true,
                    proxy_configured: true,
                    proxy_server: None,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                },
                session: None,
                proxy: None,
            },
        )
        .await
        .unwrap();
    let draft = |text: &str| QuestionDraft {
        question_id: None,
        text: text.to_owned(),
        intent: "purchase".to_owned(),
        product_refs: vec![],
        market: "global".to_owned(),
        language: "en".to_owned(),
        source: QuestionSource {
            kind: QuestionSourceKind::UserProvided,
            reference_id: None,
        },
        weight: 1,
    };
    let created = ops
        .question_create(
            &f.scope,
            CreateQuestionSet {
                idempotency_key: "question-set-create-1".into(),
                name: "Customer questions".into(),
                questions: vec![draft("HELDOUT_CANARY_QUESTION")],
            },
        )
        .await
        .expect("writer creates scoped question set");
    assert_eq!(created.evaluation_count, 1);
    let discovered = ops
        .question_discover(
            &f.scope,
            QuestionDiscoverRequest {
                question_set_id: Some(created.question_set_id),
                question_set_version_id: Some(created.question_set_version_id),
                cursor: None,
                limit: None,
            },
        )
        .await
        .expect("discover version");
    assert_eq!(discovered.questions.len(), 1);
    for item in &discovered.questions {
        match item.purpose {
            geo_domain::QuestionPurpose::FrozenEvaluation => {
                assert!(item.optimization_text.is_none())
            }
            geo_domain::QuestionPurpose::Optimization => assert!(item.optimization_text.is_some()),
        }
    }
    assert!(
        !serde_json::to_string(&discovered)
            .unwrap()
            .contains("HELDOUT_CANARY_QUESTION")
    );
    let heldout = discovered
        .questions
        .iter()
        .find(|item| item.purpose == geo_domain::QuestionPurpose::FrozenEvaluation)
        .unwrap();
    let plan = ops
        .channel_plan(
            &f.scope,
            ChannelPlanRequest {
                cycle_id: Some(f.cycle_id),
                publications: vec![],
                measurements: vec![],
                bound_measurements: vec![ChannelBoundMeasurementPlanItem {
                    account_id: measure_account_id,
                    provider: "kimi".into(),
                    model: "model".into(),
                    surface: "consumer_web".into(),
                    search_mode: "web_search".into(),
                    protocol_version: "protocol.v1".into(),
                    question: heldout.reference,
                    scheduled_at: Utc::now() + Duration::hours(1),
                    sample_ordinal: 0,
                }],
            },
        )
        .await
        .expect("heldout reference schedules without exposing its text");
    assert_eq!(plan.expected_count, 1);
    let forged = serde_json::json!({
        "account_id": f.account_ids[0], "provider":"provider","model":"model",
        "surface":"web","search_mode":"official_search","protocol_version":"protocol.v1",
        "question": heldout.reference, "scheduled_at": Utc::now(), "sample_ordinal":0,
        "purpose":"optimization"
    });
    assert!(serde_json::from_value::<ChannelBoundMeasurementPlanItem>(forged).is_err());
    let foreign_scope = TenantScope::new(
        f.scope.operator_id,
        f.scope.tenant_id,
        Some(geo_domain::ProjectId::new(Uuid::new_v4())),
    );
    assert!(
        ops.question_discover(
            &foreign_scope,
            QuestionDiscoverRequest {
                question_set_id: Some(created.question_set_id),
                question_set_version_id: Some(created.question_set_version_id),
                cursor: None,
                limit: None,
            }
        )
        .await
        .is_err()
    );
    let stale = ops
        .question_revise(
            &f.scope,
            QuestionReviseRequest {
                question_set_id: created.question_set_id,
                command: geo_domain::ReviseQuestionSet {
                    idempotency_key: "question-set-revise-1".into(),
                    base_version_id: Uuid::new_v4(),
                    name: "Customer questions".into(),
                    questions: vec![draft("HELDOUT_CANARY_QUESTION")],
                },
            },
        )
        .await
        .expect_err("revision requires exact current base");
    assert_eq!(stale.code, HostOpErrorCode::Failed);
}

#[tokio::test]
async fn discovery_is_scoped_allowlisted_and_cursors_cannot_cross_kind_or_scope() {
    let f = fixture().await;
    let ops = ops(&f);
    let source_page = ops
        .channel_discover(
            &f.scope,
            ChannelDiscoverRequest {
                kind: ChannelDiscoveryKind::PublicSources,
                cursor: None,
                limit: Some(1),
            },
        )
        .await
        .unwrap();
    assert_eq!(source_page.current_cycle_id, Some(f.cycle_id));
    assert_eq!(source_page.items.len(), 1);
    assert!(source_page.next_cursor.is_none());
    assert!(matches!(
        &source_page.items[0],
        ChannelDiscoveryItem::PublicSource { source_id, media_type, .. }
            if *source_id == f.public_source.0 && media_type == "text/plain"
    ));
    assert_ne!(f.internal_source, f.public_source.0);

    let first = ops
        .channel_discover(
            &f.scope,
            ChannelDiscoverRequest {
                kind: ChannelDiscoveryKind::Accounts,
                cursor: None,
                limit: Some(1),
            },
        )
        .await
        .unwrap();
    let cursor = first.next_cursor.unwrap();
    let other_scope = TenantScope::new(f.scope.operator_id, f.scope.tenant_id, None);
    assert_eq!(
        ops.channel_discover(
            &other_scope,
            ChannelDiscoverRequest {
                kind: ChannelDiscoveryKind::Accounts,
                cursor: Some(cursor.clone()),
                limit: Some(1),
            },
        )
        .await
        .unwrap_err()
        .code,
        HostOpErrorCode::Denied
    );
    assert_eq!(
        ops.channel_discover(
            &f.scope,
            ChannelDiscoverRequest {
                kind: ChannelDiscoveryKind::PublicSources,
                cursor: Some(cursor.clone()),
                limit: Some(1),
            },
        )
        .await
        .unwrap_err()
        .code,
        HostOpErrorCode::InvalidRequest
    );
    let second = ops
        .channel_discover(
            &f.scope,
            ChannelDiscoverRequest {
                kind: ChannelDiscoveryKind::Accounts,
                cursor: Some(cursor),
                limit: Some(1),
            },
        )
        .await
        .unwrap();
    for item in first.items.into_iter().chain(second.items) {
        let serialized = serde_json::to_string(&item).unwrap();
        for forbidden in [
            "private-identity-fixture",
            "private-proxy-fixture",
            "session",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "private account data leaked"
            );
        }
    }
}

#[tokio::test]
async fn plan_freezes_refs_manifest_paginates_and_replay_never_sends_again() {
    let f = fixture().await;
    let ops = ops(&f);
    let receipt = ops.channel_plan(&f.scope, plan(&f)).await.unwrap();
    assert_eq!(receipt.cycle_id, f.cycle_id);
    assert_eq!(receipt.expected_count, 2);
    let replay = ops.channel_plan(&f.scope, plan(&f)).await.unwrap();
    assert_eq!(receipt, replay);
    let first = ops
        .channel_manifest_read(
            &f.scope,
            ChannelManifestReadRequest {
                cycle_id: None,
                revision: None,
                cursor: None,
                limit: Some(1),
            },
        )
        .await
        .unwrap();
    assert!(first.sealed);
    assert_eq!(first.expected_count, 2);
    let cursor = first.next_cursor.clone().unwrap();
    let second = ops
        .channel_manifest_read(
            &f.scope,
            ChannelManifestReadRequest {
                cycle_id: Some(f.cycle_id),
                revision: Some(receipt.revision),
                cursor: Some(cursor.clone()),
                limit: Some(1),
            },
        )
        .await
        .unwrap();
    assert_eq!(second.items.len(), 1);
    assert_ne!(first.items[0].target_id, second.items[0].target_id);
    assert!(first.items[0].publication_lookup.is_none());
    assert!(second.items[0].publication_lookup.is_none());
    let mut tampered = cursor.clone();
    tampered.push('0');
    assert_eq!(
        ops.channel_manifest_read(
            &f.scope,
            ChannelManifestReadRequest {
                cycle_id: Some(f.cycle_id),
                revision: None,
                cursor: Some(tampered),
                limit: Some(1),
            }
        )
        .await
        .unwrap_err()
        .code,
        HostOpErrorCode::InvalidRequest
    );
    let other_scope = TenantScope::new(f.scope.operator_id, f.scope.tenant_id, None);
    assert_eq!(
        ops.channel_manifest_read(
            &other_scope,
            ChannelManifestReadRequest {
                cycle_id: Some(f.cycle_id),
                revision: None,
                cursor: Some(cursor),
                limit: Some(1),
            }
        )
        .await
        .unwrap_err()
        .code,
        HostOpErrorCode::Denied
    );
    let target_id = first.items[0].target_id;
    let deferred = ops
        .channel_target_execute(&f.scope, ChannelTargetExecuteRequest { target_id })
        .await
        .unwrap();
    assert_eq!(deferred.state, ChannelExecutionState::Deferred);
    assert_eq!(
        deferred.deferred_reason.as_deref(),
        Some("runner_unavailable")
    );

    // A claimed attempt represents an external effect that may have happened.
    // The tool must report it, never invoke the dispatcher a second time.
    let repo = f.state.channel_job_repository();
    let (_, claimed) = repo
        .claim(&f.scope, target_id, Uuid::new_v4(), Utc::now())
        .await
        .unwrap();
    let unknown = ops
        .channel_target_execute(&f.scope, ChannelTargetExecuteRequest { target_id })
        .await
        .unwrap();
    assert_eq!(unknown.state, ChannelExecutionState::UnknownResult);
    assert_eq!(unknown.attempt_id, Some(claimed.attempt_id));
    repo.finish(
        &f.scope,
        target_id,
        claimed.attempt_id,
        ChannelOutcome {
            status: ChannelOutcomeStatus::Verified,
            detail: Some("private runner detail fixture".into()),
            occurred_at: Utc::now(),
            raw_answer: Some("private raw answer fixture".into()),
            citations: vec!["private citation fixture".into()],
            public_url: Some("https://example.invalid/private-identity-fixture".into()),
            screenshot_ref: Some("private screenshot fixture".into()),
            connector_version: Some("fixture-1".into()),
            runner_evidence: vec![serde_json::json!({"private":"runner evidence"})],
            fixture: true,
        },
        Utc::now(),
    )
    .await
    .unwrap();
    let completed = ops
        .channel_target_execute(&f.scope, ChannelTargetExecuteRequest { target_id })
        .await
        .unwrap();
    assert_eq!(completed.state, ChannelExecutionState::Completed);
    assert_eq!(completed.outcome_status.as_deref(), Some("verified"));
    assert_eq!(completed.fixture, Some(true));
    let serialized = serde_json::to_string(&completed).unwrap();
    for forbidden in [
        "private runner",
        "private raw",
        "private citation",
        "private screenshot",
        "private-identity-fixture",
        "runner_evidence",
        "body",
    ] {
        assert!(!serialized.contains(forbidden), "raw evidence leaked");
    }
    assert_eq!(
        repo.get_target(&f.scope, target_id)
            .await
            .unwrap()
            .attempts
            .len(),
        1
    );
}

#[tokio::test]
async fn missing_service_and_internal_source_plan_fail_closed() {
    let f = fixture().await;
    let unconfigured =
        RepositoryHostOps::new(Arc::new(geo_domain::MemoryKnowledgeRepository::default()));
    assert_eq!(
        unconfigured
            .channel_discover(
                &f.scope,
                ChannelDiscoverRequest {
                    kind: ChannelDiscoveryKind::Accounts,
                    cursor: None,
                    limit: None
                }
            )
            .await
            .unwrap_err()
            .code,
        HostOpErrorCode::CapabilityMissing
    );
    let mut forbidden = plan(&f);
    forbidden.publications[0].source_id = f.internal_source;
    assert!(ops(&f).channel_plan(&f.scope, forbidden).await.is_err());
    let alien = TenantScope::new(f.scope.operator_id, f.scope.tenant_id, None);
    assert_eq!(
        ops(&f)
            .channel_target_execute(
                &alien,
                ChannelTargetExecuteRequest {
                    target_id: Uuid::new_v4()
                }
            )
            .await
            .unwrap_err()
            .code,
        HostOpErrorCode::Denied
    );
    let other_project = TenantScope::new(
        f.scope.operator_id,
        f.scope.tenant_id,
        Some(Uuid::new_v4().into()),
    );
    assert_eq!(
        ops(&f)
            .channel_plan(&other_project, plan(&f))
            .await
            .unwrap_err()
            .code,
        HostOpErrorCode::NotFound
    );
    let forged = serde_json::json!({
        "cycle_id":f.cycle_id,
        "publications":[],
        "measurements":[],
        "project_id":f.scope.project_id.unwrap()
    });
    assert!(serde_json::from_value::<ChannelPlanRequest>(forged).is_err());
}
