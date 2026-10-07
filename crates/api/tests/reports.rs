use async_trait::async_trait;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::SET_COOKIE},
};
use chrono::{Duration, Utc};
use geo_api::{
    AppState, CSRF_HEADER, EventBus, MemoryIdempotencyStore, MemoryOperationStore,
    RepositoryHostOps, preview_cycle_report, reduce_cycle_report, router,
};
use geo_domain::{
    AcceptContentDistributionRequest, AppError, ChannelAccount, ChannelOwnerKind, ChannelPlan,
    ChannelStatus, ChannelTarget, ChannelTargetInput, ChunkLocator, ContentBlock, ContentBlockKind,
    ContentCoverage, ContentExecution, ContentExecutionStatus, ContentHandoff, ContentHandoffItem,
    ContentItemStatus, ContentRevision, DEVELOPMENT_TENANT_ID, DEVELOPMENT_USER_EMAIL,
    DistributionRepository, DistributionTargetStatus, DocumentManifest, DocumentManifestCoverage,
    DocumentManifestItem, DocumentManifestItemState, DocumentManifestState, ErrorCode, EvidenceRef,
    FreezeDistribution, IntentVerification, Membership, MemoryAuthRepository, PlatformPlacement,
    PreparedDistribution, ProjectCreate, ProjectSettings, ProjectStartCommand,
    PublicationLookupCandidate, PublicationLookupFinding, PublicationLookupJob,
    PublicationLookupObservation, PublicationLookupReportObservation, PublicationLookupRepository,
    ReportAvailability, ReportManifestKind, Role, StructuredDocument, TEXT_DISTRIBUTION_FORMAT,
    TenantScope, User, hash_idempotency_key, prepare_content_distribution_request, settings_hash,
    sha256_hex, start_request_hash,
};
use geo_worker::{HostOps, ReportGetRequest, ReportReduceRequest};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

struct ReportLookup {
    scope: TenantScope,
    row: PublicationLookupReportObservation,
    invalid_newer: bool,
}

#[async_trait]
impl PublicationLookupRepository for ReportLookup {
    async fn enqueue(
        &self,
        _: &TenantScope,
        _: Uuid,
        _: Uuid,
        _: chrono::DateTime<Utc>,
    ) -> Result<PublicationLookupJob, AppError> {
        panic!("report enqueued lookup")
    }
    async fn scan_due(
        &self,
        _: Option<Uuid>,
        _: chrono::DateTime<Utc>,
        _: usize,
    ) -> Result<Vec<PublicationLookupCandidate>, AppError> {
        panic!("report scanned lookup")
    }
    async fn claim(
        &self,
        _: &TenantScope,
        _: Uuid,
        _: Uuid,
        _: chrono::DateTime<Utc>,
        _: chrono::DateTime<Utc>,
    ) -> Result<PublicationLookupJob, AppError> {
        panic!("report claimed lookup")
    }
    async fn finish(
        &self,
        _: &TenantScope,
        _: Uuid,
        _: PublicationLookupObservation,
        _: Option<chrono::DateTime<Utc>>,
    ) -> Result<PublicationLookupJob, AppError> {
        panic!("report finished lookup")
    }
    async fn get(&self, _: &TenantScope, _: Uuid) -> Result<PublicationLookupJob, AppError> {
        panic!("report used single-job read")
    }
    async fn observations(
        &self,
        _: &TenantScope,
        _: Uuid,
    ) -> Result<Vec<PublicationLookupObservation>, AppError> {
        panic!("report used unbounded history")
    }
    async fn observation_page(
        &self,
        _: &TenantScope,
        _: Uuid,
        _: Option<Uuid>,
        _: usize,
    ) -> Result<Vec<PublicationLookupObservation>, AppError> {
        panic!("report used UI pagination")
    }
    async fn report_asset_observations(
        &self,
        scope: &TenantScope,
        originals: &[Uuid],
        as_of: chrono::DateTime<Utc>,
    ) -> Result<Vec<PublicationLookupReportObservation>, AppError> {
        assert!(originals.len() <= 64);
        if scope != &self.scope
            || !originals.contains(&self.row.original_target_id)
            || self.row.observation.received_at > as_of
        {
            return Ok(vec![]);
        }
        let mut candidates = Vec::new();
        if self.invalid_newer {
            let mut invalid = self.row.clone();
            invalid.observation.execution_id = Uuid::new_v4();
            invalid.observation.received_at += Duration::seconds(1);
            invalid.observation.evidence["public_url"] =
                json!("https://private.example.invalid/?token=secret");
            if invalid.observation.received_at <= as_of {
                candidates.push(invalid);
            }
        }
        candidates.push(self.row.clone());
        Ok(candidates)
    }
}

fn report_lookup_row(
    target_id: Uuid,
    account_id: Uuid,
    at: chrono::DateTime<Utc>,
) -> PublicationLookupReportObservation {
    let attempt_id = Uuid::new_v4();
    let execution_id = Uuid::new_v4();
    let url = "https://www.zhihu.com/p/123456";
    let job = PublicationLookupJob {
        attempt_id,
        target_id,
        account_id,
        frozen_input: ChannelTargetInput::Publish {
            source_id: Uuid::new_v4(),
            source_version_id: Uuid::new_v4(),
            platform: "zhihu".into(),
            account_id,
            title: "Example title".into(),
            body: "Example body".into(),
            body_sha256: sha256_hex(b"Example body"),
        },
        connector_version: Some("browser.v1".into()),
        candidate_public_url: Some(url.into()),
        next_due_at: None,
        lease_execution_id: None,
        lease_expires_at: None,
        query_count: 1,
        last_error_code: None,
    };
    let observation = PublicationLookupObservation {
        execution_id,
        attempt_id,
        finding: PublicationLookupFinding::AssetObserved,
        evidence: json!({
            "schema_version":"geo.publication.asset_observation.v1",
            "provenance":"live",
            "public_url":url,
            "content_sha256":sha256_hex(b"Example title\nExample body"),
            "connector_version":"browser.v1",
            "observed_at":at.to_rfc3339(),
            "original_attempt_id":attempt_id.to_string(),
            "target_id":target_id.to_string(),
            "account_id":account_id.to_string(),
        }),
        observed_at: at,
        received_at: at,
        error_code: None,
    };
    PublicationLookupReportObservation {
        original_target_id: target_id,
        job,
        observation,
    }
}

async fn fixture() -> (
    AppState,
    Arc<MemoryAuthRepository>,
    geo_domain::ProjectId,
    Uuid,
    chrono::DateTime<chrono::Utc>,
) {
    let auth = Arc::new(MemoryAuthRepository::development_with_password(
        "test-password",
    ));
    let state = AppState::with_stores_and_auth_and_projects(
        Arc::new(MemoryOperationStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
        auth.clone(),
        Arc::new(geo_domain::MemoryProjectRepository::default()),
        EventBus::default(),
        false,
    );
    let scope = TenantScope::new(
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        None,
    );
    let settings = ProjectSettings {
        brand_name: "Example".into(),
        market: "US".into(),
        language: "en".into(),
        report_timezone: "Asia/Shanghai".into(),
        initial_sources: vec![geo_domain::InitialSource {
            kind: geo_domain::InitialSourceKind::Text,
            value: "Public product description".into(),
            visibility: geo_domain::InitialSourceVisibility::Public,
            version_ref: None,
            content_hash: None,
        }],
        ..Default::default()
    };
    let repo = state.project_repository();
    let project = repo
        .create(
            &scope,
            ProjectCreate {
                slug: Some("report-test".into()),
                display_name: "Report test".into(),
                settings,
            },
        )
        .await
        .unwrap();
    let hash = settings_hash(&project.settings).unwrap();
    let acceptance = repo
        .start(
            &scope,
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("report-test"),
                request_hash: start_request_hash(project.id, project.revision, &hash),
                settings_hash: hash,
                operation_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    let started = repo.get_start(&scope, project.id).await.unwrap().unwrap();
    assert_eq!(started.report_timezone, "Asia/Shanghai");
    (
        state,
        auth,
        project.id,
        acceptance.cycle_id,
        started.cutoff_at,
    )
}

async fn login(app: &Router, name: &str, password: &str) -> (String, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("host", "localhost:8080")
                .header("origin", "http://localhost:5173")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"login_name":name,"password":password}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()[SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 16 * 1024).await.unwrap()).unwrap();
    (cookie, body["csrf_token"].as_str().unwrap().to_owned())
}

fn request(method: &str, uri: &str, cookie: &str, csrf: Option<&str>, body: &str) -> Request<Body> {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost:8080")
        .header("origin", "http://localhost:5173")
        .header("cookie", cookie)
        .header("content-type", "application/json");
    if let Some(csrf) = csrf {
        req = req.header(CSRF_HEADER, csrf);
    }
    req.body(Body::from(body.to_owned())).unwrap()
}

async fn freeze_formal_coverage(
    state: &AppState,
    scope: &TenantScope,
    cycle_id: Uuid,
) -> (Arc<dyn DistributionRepository>, Uuid) {
    freeze_formal_coverage_with_revision(state, scope, cycle_id, None).await
}

async fn freeze_formal_coverage_with_revision(
    state: &AppState,
    scope: &TenantScope,
    cycle_id: Uuid,
    reused_revision_id: Option<Uuid>,
) -> (Arc<dyn DistributionRepository>, Uuid) {
    let project_id = scope.project_id.unwrap();
    let cycle = state
        .project_repository()
        .get_report_cycle(scope, project_id, cycle_id)
        .await
        .unwrap()
        .unwrap();
    let document_id = cycle.document_manifest.unwrap().manifest_id;
    let release_id = Uuid::new_v4();
    let items: Vec<_> = ["first", "second"]
        .into_iter()
        .map(|document_key| DocumentManifestItem {
            document_manifest_item_id: Uuid::new_v4(),
            manifest_id: document_id,
            knowledge_release_id: release_id,
            document_key: document_key.into(),
            content_type: "article".into(),
            product_id: None,
            market: "global".into(),
            language: "en".into(),
            state: DocumentManifestItemState::Planned,
            block_reason: None,
            dependency_hash: document_key.into(),
            source_version_refs: vec![],
        })
        .collect();
    let manifest = DocumentManifest {
        manifest_id: document_id,
        operator_id: scope.operator_id,
        tenant_id: scope.tenant_id,
        project_id,
        revision: 1,
        knowledge_release_id: release_id,
        planner_version: "report-fixture".into(),
        state: DocumentManifestState::Ready,
        sealed: true,
        expected_count: Some(2),
        scope_hash: "report-fixture".into(),
        items: items.clone(),
        coverage: DocumentManifestCoverage {
            total: 2,
            planned: 2,
            ..Default::default()
        },
    };
    let coverage = ContentCoverage {
        total: 2,
        ready: 1,
        blocked: 1,
        deferred: 0,
        not_applicable: 0,
        cancelled: 0,
        incomplete: 0,
    };
    let execution_id = Uuid::new_v4();
    let handoff_id = Uuid::new_v4();
    let revision_id = reused_revision_id.unwrap_or_else(Uuid::new_v4);
    let repo = state.distribution_repository();
    let frozen = repo
        .freeze(
            scope,
            FreezeDistribution {
                cycle_id,
                revision: 1,
                document_manifest: manifest,
                content_execution: ContentExecution {
                    execution_id,
                    project_id,
                    cycle_id,
                    manifest_id: document_id,
                    manifest_revision: 1,
                    policy_version: "report-fixture".into(),
                    input_hash: "report-fixture".into(),
                    status: ContentExecutionStatus::Closed,
                    expected_count: 2,
                    coverage: coverage.clone(),
                    handoff_id: Some(handoff_id),
                },
                content_handoff: ContentHandoff {
                    handoff_id,
                    execution_id,
                    revision: 1,
                    supersedes_handoff_id: None,
                    coverage,
                    items: items
                        .iter()
                        .enumerate()
                        .map(|(index, item)| ContentHandoffItem {
                            item_id: item.document_manifest_item_id,
                            document_key: item.document_key.clone(),
                            status: if index == 0 {
                                ContentItemStatus::Ready
                            } else {
                                ContentItemStatus::Blocked
                            },
                            reason: (index == 1).then(|| "source_unavailable".into()),
                            revision_id: (index == 0).then_some(revision_id),
                        })
                        .collect(),
                    created_at: Utc::now(),
                },
                placements: ["zhihu", "beta", "gamma"]
                    .into_iter()
                    .map(|platform_id| PlatformPlacement {
                        platform_id: platform_id.into(),
                        placement_slot: "primary".into(),
                        capability_version: "report-fixture".into(),
                        supported_formats: if platform_id == "gamma" {
                            vec!["faq".into()]
                        } else {
                            vec!["article".into()]
                        },
                        unavailable_reason: (platform_id == "beta")
                            .then(|| "account_unavailable".into()),
                        fixture: true,
                    })
                    .collect(),
                sealed_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    (repo, frozen.manifest_id)
}

async fn create_legacy_measurement_and_publication(
    state: &AppState,
    scope: &TenantScope,
    cycle_id: Uuid,
) -> Uuid {
    let project_id = scope.project_id.unwrap();
    let cycle = state
        .project_repository()
        .get_report_cycle(scope, project_id, cycle_id)
        .await
        .unwrap()
        .unwrap();
    let plan_id = Uuid::new_v4();
    state
        .channel_job_repository()
        .create_plan(
            scope,
            ChannelPlan {
                plan_id,
                project_id,
                cycle_id,
                input_hash: format!("plan-{plan_id}"),
                revision: 1,
                created_at: Utc::now(),
                targets: vec![
                    ChannelTarget {
                        target_id: Uuid::new_v4(),
                        input: ChannelTargetInput::Publish {
                            source_id: Uuid::new_v4(),
                            source_version_id: Uuid::new_v4(),
                            platform: "zhihu".into(),
                            account_id: Uuid::new_v4(),
                            title: "Unsent".into(),
                            body: "No public result".into(),
                            body_sha256: "fixture".into(),
                        },
                    },
                    ChannelTarget {
                        target_id: Uuid::new_v4(),
                        input: ChannelTargetInput::Measure {
                            account_id: Uuid::new_v4(),
                            provider: "provider".into(),
                            model: "fixed".into(),
                            surface: "web".into(),
                            search_mode: "search".into(),
                            protocol_version: "v1".into(),
                            question_set_version: "v1".into(),
                            question: "Unobserved question?".into(),
                            market: "global".into(),
                            language: "en".into(),
                            scheduled_at: cycle.report_window_start_at + Duration::hours(1),
                            sample_ordinal: 0,
                            question_binding: None,
                        },
                    },
                ],
            },
        )
        .await
        .unwrap();
    plan_id
}

#[tokio::test]
async fn formal_distribution_uses_frozen_two_by_three_denominator_and_partial_pages() {
    let (state, _, project_id, cycle_id, cutoff) = fixture().await;
    let scope = TenantScope::new(
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Some(project_id),
    );
    let (repo, manifest_id) = freeze_formal_coverage(&state, &scope, cycle_id).await;
    let frozen_at = repo.get(&scope, manifest_id).await.unwrap().sealed_at;
    let before_freeze = repo
        .cycle_inputs(&scope, cycle_id, frozen_at - Duration::nanoseconds(1))
        .await
        .unwrap();
    assert!(before_freeze.manifest.is_none());
    assert!(before_freeze.targets.is_empty());
    let page = repo
        .expansion_page(&scope, manifest_id, 0, 4)
        .await
        .unwrap();
    repo.commit_expansion_page(&scope, manifest_id, 0, page.rows)
        .await
        .unwrap();
    let first = reduce_cycle_report(
        &state,
        &scope,
        cycle_id,
        None,
        cutoff + Duration::seconds(1),
    )
    .await
    .unwrap();
    let reference = first
        .input_manifest_versions
        .iter()
        .find(|reference| reference.kind == ReportManifestKind::Distribution)
        .unwrap();
    assert_eq!(reference.manifest_id, manifest_id);
    assert_eq!(reference.expected_count, Some(6));
    assert_eq!(first.publications.expected_count, Some(6));
    assert_eq!(first.publications.observed_count, 4);
    assert_eq!(first.publications.counts["pending"], 1);
    assert_eq!(first.publications.counts["deferred"], 1);
    assert_eq!(first.publications.counts["not_applicable"], 1);
    assert_eq!(first.publications.counts["blocked"], 1);
    assert_eq!(first.publications.counts["unmaterialized"], 2);
    assert!(!first.publications.counts.contains_key("verified"));
    assert_eq!(
        first.measurements.availability,
        ReportAvailability::Unavailable
    );
    let rest = repo
        .expansion_page(&scope, manifest_id, 4, 2)
        .await
        .unwrap();
    repo.commit_expansion_page(&scope, manifest_id, 4, rest.rows)
        .await
        .unwrap();
    let replay = reduce_cycle_report(&state, &scope, cycle_id, None, cutoff + Duration::days(2))
        .await
        .unwrap();
    assert_eq!(replay, first);
    let corrected = reduce_cycle_report(
        &state,
        &scope,
        cycle_id,
        Some(first.report_id),
        cutoff + Duration::days(2),
    )
    .await
    .unwrap();
    assert_eq!(corrected.publications.expected_count, Some(6));
    assert_eq!(corrected.publications.observed_count, 6);
    assert_eq!(corrected.publications.counts["blocked"], 3);
    assert!(!corrected.publications.counts.contains_key("verified"));
    assert_eq!(
        reduce_cycle_report(
            &state,
            &TenantScope::new(scope.operator_id, Uuid::new_v4().into(), Some(project_id)),
            cycle_id,
            None,
            cutoff + Duration::days(2),
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn formal_manifest_frozen_after_first_report_does_not_rewrite_replay_or_correction() {
    let (state, _, project_id, cycle_id, cutoff) = fixture().await;
    let scope = TenantScope::new(
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Some(project_id),
    );
    let first = reduce_cycle_report(
        &state,
        &scope,
        cycle_id,
        None,
        cutoff + Duration::seconds(1),
    )
    .await
    .unwrap();
    freeze_formal_coverage(&state, &scope, cycle_id).await;
    assert_eq!(
        reduce_cycle_report(&state, &scope, cycle_id, None, cutoff + Duration::days(2),)
            .await
            .unwrap(),
        first
    );
    let correction = reduce_cycle_report(
        &state,
        &scope,
        cycle_id,
        Some(first.report_id),
        cutoff + Duration::days(2),
    )
    .await
    .unwrap();
    assert_eq!(
        correction.input_manifest_versions,
        first.input_manifest_versions
    );
    assert_eq!(
        correction.publications.availability,
        ReportAvailability::Unsealed
    );
}

#[tokio::test]
async fn formal_distribution_keeps_independent_measurement_without_legacy_publish_double_count() {
    let (state, _, project_id, cycle_id, cutoff) = fixture().await;
    let scope = TenantScope::new(
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Some(project_id),
    );
    let legacy_id = create_legacy_measurement_and_publication(&state, &scope, cycle_id).await;
    let (_, formal_id) = freeze_formal_coverage(&state, &scope, cycle_id).await;
    let report = reduce_cycle_report(
        &state,
        &scope,
        cycle_id,
        None,
        cutoff + Duration::seconds(1),
    )
    .await
    .unwrap();
    assert_eq!(report.publications.expected_count, Some(6));
    assert_eq!(report.publications.observed_count, 0);
    assert_eq!(report.publications.counts["unmaterialized"], 6);
    assert_eq!(report.measurements.expected_count, Some(1));
    assert_eq!(report.measurements.counts["pending"], 1);
    assert_eq!(
        report
            .input_manifest_versions
            .iter()
            .filter(|reference| reference.kind == ReportManifestKind::Distribution)
            .map(|reference| reference.manifest_id)
            .collect::<Vec<_>>(),
        vec![formal_id]
    );
    assert_eq!(
        report
            .input_manifest_versions
            .iter()
            .find(|reference| reference.kind == ReportManifestKind::Measurement)
            .unwrap()
            .manifest_id,
        legacy_id
    );
}

#[tokio::test]
async fn ready_and_reused_verified_intents_without_target_receipts_are_not_reported_success() {
    let (state, _, project_id, cycle_id, cutoff) = fixture().await;
    let scope = TenantScope::new(
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Some(project_id),
    );
    let (repo, manifest_id) = freeze_formal_coverage(&state, &scope, cycle_id).await;
    let page = repo
        .expansion_page(&scope, manifest_id, 0, 6)
        .await
        .unwrap();
    let pending = page
        .rows
        .iter()
        .find(|row| row.platform_id == "zhihu")
        .unwrap()
        .clone();
    repo.commit_expansion_page(&scope, manifest_id, 0, page.rows)
        .await
        .unwrap();
    let document = StructuredDocument {
        title: "Example".into(),
        blocks: vec![ContentBlock {
            block_id: Uuid::new_v4(),
            kind: ContentBlockKind::Paragraph,
            text: "Public source-backed text".into(),
            citation_ids: vec![],
            items: vec![],
            rich: None,
        }],
        schema_version: None,
    };
    let revision = ContentRevision {
        revision_id: pending.content_revision_id.unwrap(),
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
    let prepared = PreparedDistribution {
        manifest_id,
        target_id: pending.target_id,
        revision: Some(revision),
        account_id: Some(Uuid::new_v4()),
        defer_reason: None,
    };
    let ready = repo.materialize(&scope, prepared.clone()).await.unwrap();
    assert_eq!(ready.target.status, DistributionTargetStatus::Ready);
    let intent = ready.intent.unwrap();
    repo.record_intent_verification(
        &scope,
        intent.intent_id,
        IntentVerification::Verified,
        Uuid::new_v4(),
    )
    .await
    .unwrap();
    let reused = repo.materialize(&scope, prepared).await.unwrap();
    assert_eq!(
        reused.target.status,
        DistributionTargetStatus::ReusedVerified
    );
    let report = reduce_cycle_report(
        &state,
        &scope,
        cycle_id,
        None,
        cutoff + Duration::seconds(1),
    )
    .await
    .unwrap();
    assert_eq!(report.publications.counts["unknown"], 1);
    assert!(!report.publications.counts.contains_key("verified"));
    assert!(report.findings.iter().any(|finding| {
        finding.kind == "publication_unknown"
            && finding
                .insufficient_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("target-associated"))
    }));
}

#[tokio::test]
async fn legacy_publications_are_used_only_when_formal_distribution_is_absent() {
    let (state, _, project_id, cycle_id, cutoff) = fixture().await;
    let scope = TenantScope::new(
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Some(project_id),
    );
    let legacy_id = create_legacy_measurement_and_publication(&state, &scope, cycle_id).await;
    let report = reduce_cycle_report(
        &state,
        &scope,
        cycle_id,
        None,
        cutoff + Duration::seconds(1),
    )
    .await
    .unwrap();
    assert_eq!(report.publications.expected_count, Some(1));
    assert_eq!(report.publications.counts["pending"], 1);
    assert_eq!(report.measurements.expected_count, Some(1));
    assert_eq!(
        report
            .input_manifest_versions
            .iter()
            .find(|reference| reference.kind == ReportManifestKind::Distribution)
            .unwrap()
            .manifest_id,
        legacy_id
    );
}

#[tokio::test]
async fn legacy_lookup_asset_is_late_only_in_explicit_correction_and_preserves_unknown() {
    let (state, _, project_id, cycle_id, cutoff) = fixture().await;
    let scope = TenantScope::new(
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Some(project_id),
    );
    create_legacy_measurement_and_publication(&state, &scope, cycle_id).await;
    let plan = state
        .channel_job_repository()
        .get_plan(&scope, cycle_id)
        .await
        .unwrap()
        .unwrap();
    let target = &plan.targets[0];
    let attempt_id = Uuid::new_v4();
    let sent_at = cutoff - Duration::minutes(5);
    state
        .channel_job_repository()
        .claim(&scope, target.target_id, attempt_id, sent_at)
        .await
        .unwrap();
    state
        .channel_job_repository()
        .finish(
            &scope,
            target.target_id,
            attempt_id,
            geo_domain::ChannelOutcome {
                status: geo_domain::ChannelOutcomeStatus::Unknown,
                detail: None,
                occurred_at: sent_at,
                raw_answer: None,
                citations: vec![],
                public_url: None,
                screenshot_ref: None,
                connector_version: Some("browser.v1".into()),
                runner_evidence: vec![],
                fixture: false,
            },
            sent_at,
        )
        .await
        .unwrap();
    let row = report_lookup_row(
        target.target_id,
        target.input.account_id(),
        cutoff + Duration::seconds(1),
    );
    let state = state.with_publication_lookup_repository(Arc::new(ReportLookup {
        scope: scope.clone(),
        row,
        invalid_newer: true,
    }));
    let first = reduce_cycle_report(
        &state,
        &scope,
        cycle_id,
        None,
        cutoff + Duration::seconds(2),
    )
    .await
    .unwrap();
    assert_eq!(first.publications.counts["unknown"], 1);
    assert!(
        !first
            .findings
            .iter()
            .any(|f| f.kind == "publication_asset_observed")
    );
    let replay = reduce_cycle_report(&state, &scope, cycle_id, None, cutoff + Duration::days(1))
        .await
        .unwrap();
    assert_eq!(first, replay);
    let correction = reduce_cycle_report(
        &state,
        &scope,
        cycle_id,
        Some(first.report_id),
        cutoff + Duration::days(1),
    )
    .await
    .unwrap();
    assert_eq!(correction.publications.counts["unknown"], 1);
    assert!(
        correction
            .findings
            .iter()
            .any(|f| f.kind == "publication_asset_observed")
    );
    assert!(correction.evidence.iter().any(|e| {
        e.kind == "publication_lookup_asset_observed" && e.resource_id == target.target_id
    }));
    assert_eq!(
        correction
            .evidence
            .iter()
            .filter(|e| e.kind == "publication_lookup_asset_observed")
            .count(),
        1
    );
    let json = serde_json::to_string(&correction).unwrap();
    assert!(!json.contains("https://"));
    assert!(!json.contains("account_id"));
    assert!(!json.contains("content_sha256"));
}

#[tokio::test]
async fn formal_reused_intent_maps_lookup_to_frozen_cell_not_original_send_id() {
    let (state, _, project_id, cycle_id, cutoff) = fixture().await;
    let scope = TenantScope::new(
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Some(project_id),
    );
    let (repo, manifest_id) = freeze_formal_coverage(&state, &scope, cycle_id).await;
    let page = repo
        .expansion_page(&scope, manifest_id, 0, 6)
        .await
        .unwrap();
    let target = page
        .rows
        .iter()
        .find(|row| row.platform_id == "zhihu")
        .unwrap()
        .clone();
    repo.commit_expansion_page(&scope, manifest_id, 0, page.rows)
        .await
        .unwrap();
    let document = StructuredDocument {
        title: "Example title".into(),
        blocks: vec![ContentBlock {
            block_id: Uuid::new_v4(),
            kind: ContentBlockKind::Paragraph,
            text: "Example body".into(),
            citation_ids: vec![],
            items: vec![],
            rich: None,
        }],
        schema_version: None,
    };
    let revision = ContentRevision {
        revision_id: target.content_revision_id.unwrap(),
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
    let account_id = Uuid::new_v4();
    let prepared = PreparedDistribution {
        manifest_id,
        target_id: target.target_id,
        revision: Some(revision.clone()),
        account_id: Some(account_id),
        defer_reason: None,
    };
    let ready = repo.materialize(&scope, prepared).await.unwrap();
    let original = repo
        .get_publication_bundle(&scope, ready.intent.unwrap().intent_id)
        .await
        .unwrap();
    assert_eq!(original.intent.channel_target_id, target.target_id);
    let first = reduce_cycle_report(
        &state,
        &scope,
        cycle_id,
        None,
        cutoff + Duration::seconds(1),
    )
    .await
    .unwrap();
    assert_eq!(first.publications.counts["pending"], 1);
    assert!(
        !first
            .findings
            .iter()
            .any(|finding| finding.kind == "publication_asset_observed")
    );
    let next_cycle = state
        .project_repository()
        .schedule_next_cycle(&scope, project_id, cycle_id, cutoff + Duration::seconds(1))
        .await
        .unwrap();
    let (_, second_manifest) = freeze_formal_coverage_with_revision(
        &state,
        &scope,
        next_cycle.cycle_id,
        Some(revision.revision_id),
    )
    .await;
    let second_page = repo
        .expansion_page(&scope, second_manifest, 0, 6)
        .await
        .unwrap();
    let second_target = second_page
        .rows
        .iter()
        .find(|row| row.platform_id == "zhihu")
        .unwrap()
        .clone();
    assert_ne!(second_target.target_id, target.target_id);
    repo.commit_expansion_page(&scope, second_manifest, 0, second_page.rows)
        .await
        .unwrap();
    let reused = repo
        .materialize(
            &scope,
            PreparedDistribution {
                manifest_id: second_manifest,
                target_id: second_target.target_id,
                revision: Some(revision),
                account_id: Some(account_id),
                defer_reason: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        reused.target.status,
        DistributionTargetStatus::ReusedUnknown
    );
    assert_eq!(reused.intent.unwrap().channel_target_id, target.target_id);
    let row = report_lookup_row(
        original.command.command_id,
        original.intent.account_id,
        cutoff - Duration::seconds(1),
    );
    let state = state.with_publication_lookup_repository(Arc::new(ReportLookup {
        scope: scope.clone(),
        row,
        invalid_newer: false,
    }));
    let report = reduce_cycle_report(
        &state,
        &scope,
        next_cycle.cycle_id,
        None,
        next_cycle.cutoff_at + Duration::seconds(1),
    )
    .await
    .unwrap();
    assert_eq!(report.publications.counts["unknown"], 1);
    let asset = report
        .evidence
        .iter()
        .find(|e| e.kind == "publication_lookup_asset_observed")
        .unwrap();
    assert_eq!(asset.resource_id, second_target.target_id);
    assert!(
        report
            .findings
            .iter()
            .any(|f| f.kind == "publication_asset_observed")
    );
}

#[tokio::test]
async fn formal_cell_reusing_independent_request_maps_lookup_to_real_command_id() {
    let (state, _, project_id, cycle_id, cutoff) = fixture().await;
    let scope = TenantScope::new(
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Some(project_id),
    );
    let (repo, manifest_id) = freeze_formal_coverage(&state, &scope, cycle_id).await;
    let page = repo
        .expansion_page(&scope, manifest_id, 0, 6)
        .await
        .unwrap();
    let target = page
        .rows
        .iter()
        .find(|row| row.platform_id == "zhihu")
        .unwrap()
        .clone();
    repo.commit_expansion_page(&scope, manifest_id, 0, page.rows)
        .await
        .unwrap();
    let document = StructuredDocument {
        title: "Independent article".into(),
        blocks: vec![ContentBlock {
            block_id: Uuid::new_v4(),
            kind: ContentBlockKind::Paragraph,
            text: "Evidence-backed body".into(),
            citation_ids: vec![],
            items: vec![],
            rich: None,
        }],
        schema_version: None,
    };
    let revision = ContentRevision {
        revision_id: target.content_revision_id.unwrap(),
        asset_id: Uuid::new_v4(),
        revision: 1,
        base_revision_id: None,
        derived_from_revision_id: None,
        markdown: document.markdown(),
        document,
        evidence: vec![EvidenceRef {
            source_version_id: Uuid::new_v4(),
            chunk_id: Some(Uuid::new_v4()),
            locator: ChunkLocator::Text {
                start_line: 1,
                end_line: 1,
                start_char: 0,
                end_char: 4,
            },
        }],
        quotes: vec![],
        findings: vec![],
        created_at: Utc::now(),
    };
    let account = ChannelAccount {
        account_id: Uuid::new_v4(),
        project_id,
        owner_kind: ChannelOwnerKind::Customer,
        platform: target.platform_id.clone(),
        group_id: None,
        status: ChannelStatus::Ready,
        display_name: None,
        platform_account_id: None,
        avatar_url: None,
        enabled: true,
        proxy_configured: false,
        proxy_server: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let request = prepare_content_distribution_request(
        &scope,
        &AcceptContentDistributionRequest {
            revision: revision.clone(),
            account: account.clone(),
            placement_slot: target.placement_slot.clone(),
            format: TEXT_DISTRIBUTION_FORMAT.into(),
            idempotency_key: "independent-report-fixture".into(),
        },
    )
    .unwrap();
    let original = repo
        .materialize_request_origin(&scope, &request, &revision)
        .await
        .unwrap();
    assert!(original.channel_target_id.is_nil());
    let command = repo
        .get_publication_bundle(&scope, original.intent_id)
        .await
        .unwrap()
        .command
        .command_id;
    let reused = repo
        .materialize(
            &scope,
            PreparedDistribution {
                manifest_id,
                target_id: target.target_id,
                revision: Some(revision),
                account_id: Some(account.account_id),
                defer_reason: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(reused.intent.unwrap().intent_id, original.intent_id);
    assert_eq!(
        repo.list_targets(&scope, manifest_id, None, 100)
            .await
            .unwrap()
            .expected_count,
        6
    );
    let state = state.with_publication_lookup_repository(Arc::new(ReportLookup {
        scope: scope.clone(),
        row: report_lookup_row(command, account.account_id, cutoff - Duration::seconds(1)),
        invalid_newer: false,
    }));
    let report = reduce_cycle_report(
        &state,
        &scope,
        cycle_id,
        None,
        cutoff + Duration::seconds(1),
    )
    .await
    .unwrap();
    assert_eq!(report.publications.expected_count, Some(6));
    assert!(report.evidence.iter().any(|e| {
        e.kind == "publication_lookup_asset_observed" && e.resource_id == target.target_id
    }));
}

#[tokio::test]
async fn due_report_replays_immutable_snapshot_and_keeps_absent_sources_unavailable() {
    let (state, _, project_id, cycle_id, cutoff) = fixture().await;
    let scope = TenantScope::new(
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Some(project_id),
    );
    let early = reduce_cycle_report(
        &state,
        &scope,
        cycle_id,
        None,
        cutoff - Duration::seconds(1),
    )
    .await
    .unwrap_err();
    assert_eq!(early.code, ErrorCode::NotReady);
    let first = reduce_cycle_report(
        &state,
        &scope,
        cycle_id,
        None,
        cutoff + Duration::seconds(1),
    )
    .await
    .unwrap();
    assert_eq!(first.revision, 1);
    assert_eq!(first.report_timezone, "Asia/Shanghai");
    assert_eq!(first.documents.availability, ReportAvailability::Unsealed);
    assert_eq!(
        first.publications.availability,
        ReportAvailability::Unsealed
    );
    assert_eq!(
        first.measurements.availability,
        ReportAvailability::Unavailable
    );
    assert_eq!(first.documents.expected_count, None);
    assert!(first.evidence.is_empty());
    let replay = reduce_cycle_report(&state, &scope, cycle_id, None, cutoff + Duration::days(2))
        .await
        .unwrap();
    assert_eq!(replay, first);
    let corrected = reduce_cycle_report(
        &state,
        &scope,
        cycle_id,
        Some(first.report_id),
        cutoff + Duration::days(2),
    )
    .await
    .unwrap();
    assert_eq!(corrected.revision, 2);
    assert_eq!(corrected.correction_of, Some(first.report_id));
    assert_eq!(
        corrected.report_id,
        reduce_cycle_report(
            &state,
            &scope,
            cycle_id,
            Some(first.report_id),
            cutoff + Duration::days(3)
        )
        .await
        .unwrap()
        .report_id
    );
    assert_eq!(
        state
            .report_repository()
            .list(&scope, project_id)
            .await
            .unwrap()
            .len(),
        2
    );
    let tools =
        RepositoryHostOps::new(state.knowledge_repository()).with_report_state(state.clone());
    assert_eq!(
        tools
            .report_get(&scope, ReportGetRequest { report_id: None })
            .await
            .unwrap()
            .report_id,
        corrected.report_id
    );
    assert_eq!(
        tools
            .report_reduce(
                &scope,
                ReportReduceRequest {
                    cycle_id: Some(cycle_id),
                    correction_of: None,
                }
            )
            .await
            .unwrap()
            .report_id,
        first.report_id
    );
    // Persisting the first snapshot now advances the project to its next
    // cycle. An implicit reduction targets that cycle and must not pretend
    // its future cutoff has arrived or silently return an old period.
    assert!(
        tools
            .report_reduce(
                &scope,
                ReportReduceRequest {
                    cycle_id: None,
                    correction_of: None,
                },
            )
            .await
            .is_err()
    );
    let other_scope = TenantScope::new(
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        Uuid::new_v4().into(),
        Some(project_id),
    );
    assert_eq!(
        reduce_cycle_report(
            &state,
            &other_scope,
            cycle_id,
            None,
            cutoff + Duration::days(1)
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::NotFound
    );
    assert!(
        tools
            .report_get(
                &other_scope,
                ReportGetRequest {
                    report_id: Some(first.report_id)
                }
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn preview_preserves_frozen_window_and_never_persists_or_advances_cycle() {
    let (state, _, project_id, cycle_id, cutoff) = fixture().await;
    let scope = TenantScope::new(
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Some(project_id),
    );
    let cycle = state
        .project_repository()
        .get_report_cycle(&scope, project_id, cycle_id)
        .await
        .unwrap()
        .unwrap();
    let (distribution, manifest_id) = freeze_formal_coverage(&state, &scope, cycle_id).await;
    let page = distribution
        .expansion_page(&scope, manifest_id, 0, 4)
        .await
        .unwrap();
    distribution
        .commit_expansion_page(&scope, manifest_id, 0, page.rows)
        .await
        .unwrap();
    let before = state
        .project_repository()
        .get_current_cycle(&scope, project_id)
        .await
        .unwrap();
    let early = cutoff - Duration::seconds(1);
    for _ in 0..2 {
        let preview = preview_cycle_report(&state, &scope, cycle_id, early)
            .await
            .unwrap();
        let serialized = serde_json::to_value(preview).unwrap();
        assert_eq!(serialized["kind"], "preview");
        assert_eq!(serialized["generated_at"], json!(early));
        assert_eq!(serialized["evidence_as_of"], json!(early));
        assert_eq!(
            serialized["report_window_start_at"],
            json!(cycle.report_window_start_at)
        );
        assert_eq!(
            serialized["report_window_end_at"],
            json!(cycle.report_window_end_at)
        );
        assert_eq!(serialized["cutoff_at"], json!(cutoff));
        assert_eq!(serialized["publications"]["expected_count"], 6);
        assert_eq!(serialized["publications"]["observed_count"], 4);
        assert!(serialized.get("report_id").is_none());
        assert!(serialized.get("revision").is_none());
        assert!(serialized.get("correction_of").is_none());
    }
    assert!(
        state
            .report_repository()
            .list(&scope, project_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        state
            .project_repository()
            .get_current_cycle(&scope, project_id)
            .await
            .unwrap(),
        before
    );
    assert_eq!(
        reduce_cycle_report(&state, &scope, cycle_id, None, early)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotReady
    );
}

#[tokio::test]
async fn preview_at_or_after_cutoff_excludes_late_asset_observations() {
    let (state, _, project_id, cycle_id, cutoff) = fixture().await;
    let scope = TenantScope::new(
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Some(project_id),
    );
    create_legacy_measurement_and_publication(&state, &scope, cycle_id).await;
    let plan = state
        .channel_job_repository()
        .get_plan(&scope, cycle_id)
        .await
        .unwrap()
        .unwrap();
    let target = &plan.targets[0];
    let row = report_lookup_row(
        target.target_id,
        target.input.account_id(),
        cutoff + Duration::seconds(1),
    );
    let state = state.with_publication_lookup_repository(Arc::new(ReportLookup {
        scope: scope.clone(),
        row,
        invalid_newer: false,
    }));
    let preview = preview_cycle_report(&state, &scope, cycle_id, cutoff + Duration::days(1))
        .await
        .unwrap();
    assert_eq!(preview.evidence_as_of, cutoff);
    assert_eq!(preview.generated_at, cutoff + Duration::days(1));
    assert_eq!(preview.publications.expected_count, Some(1));
    assert_eq!(preview.measurements.expected_count, Some(1));
    assert!(
        !preview
            .findings
            .iter()
            .any(|f| f.kind == "publication_asset_observed")
    );
    assert!(
        preview
            .evidence
            .iter()
            .all(|e| e.kind != "publication_lookup_asset_observed")
    );
    assert!(
        state
            .report_repository()
            .list(&scope, project_id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn viewer_can_preview_before_cutoff_without_creating_report_or_successor() {
    let (state, auth, project_id, cycle_id, cutoff) = fixture().await;
    assert!(Utc::now() < cutoff, "fixture must have a future cutoff");
    let scope = TenantScope::new(
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Some(project_id),
    );
    let viewer = User::new(
        Uuid::new_v4().into(),
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        "preview-viewer@localhost",
        "Viewer",
        "viewer-password",
    )
    .unwrap();
    auth.insert_user(viewer.clone()).await.unwrap();
    auth.insert_membership(Membership::new(
        viewer.id,
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Role::CustomerReadOnly,
    ))
    .await
    .unwrap();
    let app = router(state.clone());
    let (cookie, csrf) = login(&app, "preview-viewer@localhost", "viewer-password").await;
    let uri = format!(
        "/api/v1/cycles/{cycle_id}/report-preview?tenant_id={DEVELOPMENT_TENANT_ID}&project_id={project_id}"
    );
    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(request("GET", &uri, &cookie, None, ""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 128 * 1024).await.unwrap())
                .unwrap();
        assert_eq!(body["kind"], "preview");
        assert_eq!(body["cycle_id"], cycle_id.to_string());
        assert!(body.get("report_id").is_none());
        assert!(body.get("revision").is_none());
        assert!(body.get("correction_of").is_none());
    }
    assert!(
        state
            .report_repository()
            .list(&scope, project_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        state
            .project_repository()
            .get_current_cycle(&scope, project_id)
            .await
            .unwrap()
            .unwrap()
            .cycle_id,
        cycle_id
    );
    let forbidden = app
        .clone()
        .oneshot(request(
            "POST",
            &format!(
                "/api/v1/cycles/{cycle_id}/reductions?tenant_id={DEVELOPMENT_TENANT_ID}&project_id={project_id}"
            ),
            &cookie,
            Some(&csrf),
            "{}",
        ))
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    let cross_project = app
        .clone()
        .oneshot(request(
            "GET",
            &format!(
                "/api/v1/cycles/{cycle_id}/report-preview?tenant_id={DEVELOPMENT_TENANT_ID}&project_id={}",
                Uuid::new_v4()
            ),
            &cookie,
            None,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(cross_project.status(), StatusCode::NOT_FOUND);
    let cross_cycle = app
        .oneshot(request(
            "GET",
            &format!(
                "/api/v1/cycles/{}/report-preview?tenant_id={DEVELOPMENT_TENANT_ID}&project_id={project_id}",
                Uuid::new_v4()
            ),
            &cookie,
            None,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(cross_cycle.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn http_reads_are_scoped_and_viewer_cannot_trigger_reduction() {
    let (state, auth, project_id, cycle_id, cutoff) = fixture().await;
    let scope = TenantScope::new(
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Some(project_id),
    );
    let report = reduce_cycle_report(
        &state,
        &scope,
        cycle_id,
        None,
        cutoff + Duration::seconds(1),
    )
    .await
    .unwrap();
    let viewer = User::new(
        Uuid::new_v4().into(),
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        "report-viewer@localhost",
        "Viewer",
        "viewer-password",
    )
    .unwrap();
    auth.insert_user(viewer.clone()).await.unwrap();
    auth.insert_membership(Membership::new(
        viewer.id,
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Role::CustomerReadOnly,
    ))
    .await
    .unwrap();
    let app = router(state);
    let (cookie, csrf) = login(&app, "report-viewer@localhost", "viewer-password").await;
    let selector = format!("tenant_id={DEVELOPMENT_TENANT_ID}&project_id={project_id}");
    let list = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/projects/{project_id}/reports?tenant_id={DEVELOPMENT_TENANT_ID}"),
            &cookie,
            None,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let list: Value =
        serde_json::from_slice(&to_bytes(list.into_body(), 128 * 1024).await.unwrap()).unwrap();
    assert_eq!(list["items"][0]["report_id"], report.report_id.to_string());
    let detail = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/reports/{}?{selector}", report.report_id),
            &cookie,
            None,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(detail.status(), StatusCode::OK);
    let evidence = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/reports/{}/evidence?{selector}", report.report_id),
            &cookie,
            None,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(evidence.status(), StatusCode::OK);
    let evidence: Value =
        serde_json::from_slice(&to_bytes(evidence.into_body(), 16 * 1024).await.unwrap()).unwrap();
    assert_eq!(evidence, json!({"items":[]}));
    let forbidden = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/cycles/{cycle_id}/reductions?{selector}"),
            &cookie,
            Some(&csrf),
            "{}",
        ))
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    let (writer_cookie, writer_csrf) = login(&app, DEVELOPMENT_USER_EMAIL, "test-password").await;
    let replay = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/cycles/{cycle_id}/reductions?{selector}"),
            &writer_cookie,
            Some(&writer_csrf),
            "{}",
        ))
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    let replay: Value =
        serde_json::from_slice(&to_bytes(replay.into_body(), 128 * 1024).await.unwrap()).unwrap();
    assert_eq!(replay["report_id"], report.report_id.to_string());
    let synthetic = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/cycles/{cycle_id}/reductions?{selector}"),
            &writer_cookie,
            Some(&writer_csrf),
            r#"{"measurements":[{"status":"observed"}]}"#,
        ))
        .await
        .unwrap();
    assert_eq!(synthetic.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let missing = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/reports/{}?{selector}", Uuid::new_v4()),
            &cookie,
            None,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let wrong_project = app
        .oneshot(request(
            "GET",
            &format!(
                "/api/v1/reports/{}?tenant_id={DEVELOPMENT_TENANT_ID}&project_id={}",
                report.report_id,
                Uuid::new_v4()
            ),
            &cookie,
            None,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(wrong_project.status(), StatusCode::NOT_FOUND);
}
