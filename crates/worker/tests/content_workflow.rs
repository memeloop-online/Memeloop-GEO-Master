//! The real generated MemeLoop agent-agent-loop entry in embedded V8.
//! Run after `pnpm agent:bundle`: cargo test -p geo-worker --test content_workflow -- --ignored
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use geo_domain::{ContentCoverage, ContentExecutionStatus, DistributionTargetStatus};
use geo_domain::{ContentItemStatus, KnowledgeSearchRequest, KnowledgeSearchResult, TenantScope};
use geo_worker::{
    ContentCloseRequest, ContentExecutionReadRequest, ContentExecutionRef, ContentHandoffRef,
    ContentItemRef, ContentItemsPage, ContentItemsReadRequest, ContentStepRequest,
    DistributionManifestRef, DistributionResumeRequest, DistributionStartRequest,
    DistributionTargetRef, DistributionTargetsPage, DistributionTargetsReadRequest, HostBridge,
    HostOp, HostOpError, HostOps, HostRuntime, ManifestPage, ManifestReadRequest, MeasureRequest,
    MeasureSample, ModelCompletion, ModelCompletionRequest, PublishReceipt, PublishRequest,
};
use uuid::Uuid;

const ENTRY: &str = "memeloop://bundle/content-workflow.mjs";
const DEADLINE: Duration = Duration::from_secs(30);

struct Fixture {
    execution_id: Uuid,
    cycle_id: Uuid,
    manifest_id: Uuid,
    handoff_id: Uuid,
    items: Mutex<Vec<ContentItemRef>>,
    generated: Mutex<Vec<Uuid>>,
    interrupt_once: Mutex<bool>,
    expansion: Mutex<u64>,
    interrupt_expansion_once: Mutex<bool>,
    resume_pages: Mutex<Vec<Option<u64>>>,
}

impl Fixture {
    fn new() -> Self {
        let execution_id = Uuid::new_v4();
        Self {
            execution_id,
            cycle_id: Uuid::new_v4(),
            manifest_id: Uuid::new_v4(),
            handoff_id: Uuid::new_v4(),
            items: Mutex::new(
                (1..=2)
                    .map(|n| ContentItemRef {
                        item_id: Uuid::from_u128(n),
                        branch_key: format!("cycle:1:document-{n}"),
                        status: ContentItemStatus::Pending,
                        automatic_repair_count: 0,
                    })
                    .collect(),
            ),
            generated: Mutex::new(Vec::new()),
            interrupt_once: Mutex::new(true),
            expansion: Mutex::new(0),
            interrupt_expansion_once: Mutex::new(false),
            resume_pages: Mutex::new(Vec::new()),
        }
    }
    fn distribution(&self) -> DistributionManifestRef {
        let expansion_cursor = *self.expansion.lock().unwrap();
        DistributionManifestRef {
            manifest_id: self.manifest_id,
            cycle_id: self.cycle_id,
            revision: 1,
            document_manifest_id: Uuid::new_v4(),
            content_execution_id: self.execution_id,
            content_handoff_id: self.handoff_id,
            expected_count: 300,
            expansion_cursor,
            complete: expansion_cursor == 300,
        }
    }
    fn step(
        &self,
        request: ContentStepRequest,
        from: ContentItemStatus,
        to: ContentItemStatus,
    ) -> Result<ContentItemRef, HostOpError> {
        assert_eq!(request.execution_id, self.execution_id);
        let mut items = self.items.lock().unwrap();
        let item = items
            .iter_mut()
            .find(|item| item.item_id == request.item_id)
            .unwrap();
        if item.status == from {
            item.status = to;
        }
        Ok(item.clone())
    }
}

#[async_trait]
impl HostOps for Fixture {
    async fn content_execution_read(
        &self,
        _scope: &TenantScope,
        request: ContentExecutionReadRequest,
    ) -> Result<ContentExecutionRef, HostOpError> {
        assert_eq!(request.execution_id, self.execution_id);
        Ok(ContentExecutionRef {
            execution_id: self.execution_id,
            cycle_id: self.cycle_id,
            status: if self
                .items
                .lock()
                .unwrap()
                .iter()
                .all(|item| item.status == ContentItemStatus::Ready)
            {
                ContentExecutionStatus::Closed
            } else {
                ContentExecutionStatus::Running
            },
            coverage: ContentCoverage {
                total: 2,
                ready: 2,
                blocked: 0,
                deferred: 0,
                not_applicable: 0,
                cancelled: 0,
                incomplete: 0,
            },
        })
    }
    async fn distribution_start(
        &self,
        _scope: &TenantScope,
        request: DistributionStartRequest,
    ) -> Result<DistributionManifestRef, HostOpError> {
        assert_eq!(
            request.cycle_id,
            Some(self.cycle_id),
            "must not drift to current cycle"
        );
        let mut expansion = self.expansion.lock().unwrap();
        *expansion = (*expansion).max(64);
        drop(expansion);
        Ok(self.distribution())
    }
    async fn distribution_resume(
        &self,
        _scope: &TenantScope,
        request: DistributionResumeRequest,
    ) -> Result<DistributionManifestRef, HostOpError> {
        assert_eq!(request.manifest_id, self.manifest_id);
        self.resume_pages
            .lock()
            .unwrap()
            .push(request.after_ordinal);
        let mut expansion = self.expansion.lock().unwrap();
        if *expansion < 300 {
            *expansion = (*expansion + 64).min(300);
            if *self.interrupt_expansion_once.lock().unwrap() && *expansion >= 128 {
                *self.interrupt_expansion_once.lock().unwrap() = false;
                return Err(HostOpError::failed(
                    HostOp::DistributionResume,
                    "interrupted after frozen page",
                ));
            }
        }
        drop(expansion);
        Ok(self.distribution())
    }
    async fn distribution_targets_read(
        &self,
        _scope: &TenantScope,
        request: DistributionTargetsReadRequest,
    ) -> Result<DistributionTargetsPage, HostOpError> {
        assert_eq!(request.manifest_id, self.manifest_id);
        let start = request.after_ordinal.map_or(0, |ordinal| ordinal + 1);
        let end =
            (start + u64::from(request.limit.unwrap_or(25))).min(*self.expansion.lock().unwrap());
        Ok(DistributionTargetsPage {
            manifest_id: self.manifest_id,
            expected_count: 300,
            items: (start..end)
                .map(|ordinal| DistributionTargetRef {
                    target_id: Uuid::from_u128(u128::from(ordinal) + 1_000),
                    ordinal,
                    document_item_id: Uuid::from_u128(ordinal as u128 % 2 + 1),
                    content_revision_id: None,
                    platform_id: "test-placement".into(),
                    variant_id: None,
                    publication_intent_id: None,
                    status: DistributionTargetStatus::Deferred,
                    reason: Some("connector_unverified".into()),
                })
                .collect(),
            next_ordinal: (end > start && end < *self.expansion.lock().unwrap()).then_some(end - 1),
        })
    }
    async fn content_items_read(
        &self,
        _scope: &TenantScope,
        request: ContentItemsReadRequest,
    ) -> Result<ContentItemsPage, HostOpError> {
        assert_eq!(request.execution_id, self.execution_id);
        assert_eq!(request.cursor, None);
        Ok(ContentItemsPage {
            execution_id: self.execution_id,
            total: 2,
            items: self.items.lock().unwrap().clone(),
            next_cursor: None,
        })
    }
    async fn content_prepare(
        &self,
        _scope: &TenantScope,
        request: ContentStepRequest,
    ) -> Result<ContentItemRef, HostOpError> {
        self.step(
            request,
            ContentItemStatus::Pending,
            ContentItemStatus::Prepared,
        )
    }
    async fn content_generate(
        &self,
        scope: &TenantScope,
        request: ContentStepRequest,
    ) -> Result<ContentItemRef, HostOpError> {
        let item = self.step(
            request.clone(),
            ContentItemStatus::Prepared,
            ContentItemStatus::Drafted,
        )?;
        if !self.generated.lock().unwrap().contains(&request.item_id) {
            self.generated.lock().unwrap().push(request.item_id);
            // This stand-in goes through the provider host capability; the
            // production ContentService injects that provider after a lease.
            self.model_complete(
                scope,
                ModelCompletionRequest {
                    prompt: "Generate one cited document".to_owned(),
                    system: None,
                    model: None,
                    max_output_tokens: None,
                    messages: Vec::new(),
                    tools: Vec::new(),
                },
            )
            .await?;
        }
        if request.item_id == Uuid::from_u128(1) && *self.interrupt_once.lock().unwrap() {
            *self.interrupt_once.lock().unwrap() = false;
            return Err(HostOpError::failed(
                HostOp::ContentGenerate,
                "interrupted after durable generation",
            ));
        }
        Ok(item)
    }
    async fn content_check(
        &self,
        _scope: &TenantScope,
        request: ContentStepRequest,
    ) -> Result<ContentItemRef, HostOpError> {
        let item = self
            .items
            .lock()
            .unwrap()
            .iter()
            .find(|item| item.item_id == request.item_id)
            .unwrap()
            .clone();
        let to = if request.item_id == Uuid::from_u128(1) && item.automatic_repair_count < 2 {
            ContentItemStatus::NeedsRepair
        } else {
            ContentItemStatus::Ready
        };
        self.step(request, ContentItemStatus::Drafted, to)
    }
    async fn content_repair(
        &self,
        _scope: &TenantScope,
        request: ContentStepRequest,
    ) -> Result<ContentItemRef, HostOpError> {
        let mut items = self.items.lock().unwrap();
        let item = items
            .iter_mut()
            .find(|item| item.item_id == request.item_id)
            .unwrap();
        if item.status != ContentItemStatus::NeedsRepair || item.automatic_repair_count >= 2 {
            return Err(HostOpError::denied(
                HostOp::ContentRepair,
                "repair is not eligible",
            ));
        }
        item.status = ContentItemStatus::Drafted;
        item.automatic_repair_count += 1;
        Ok(item.clone())
    }
    async fn content_close(
        &self,
        _scope: &TenantScope,
        request: ContentCloseRequest,
    ) -> Result<ContentHandoffRef, HostOpError> {
        if self.items.lock().unwrap().iter().any(|item| {
            !matches!(
                item.status,
                ContentItemStatus::Ready
                    | ContentItemStatus::Blocked
                    | ContentItemStatus::Deferred
                    | ContentItemStatus::NotApplicable
            )
        }) {
            return Err(HostOpError::failed(HostOp::ContentClose, "incomplete"));
        }
        Ok(ContentHandoffRef {
            execution_id: request.execution_id,
            handoff_id: self.handoff_id,
            total: 2,
        })
    }
    async fn model_complete(
        &self,
        _scope: &TenantScope,
        _request: ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError> {
        Ok(ModelCompletion {
            text: "a cited document".to_owned(),
            model: "injected-model".to_owned(),
            prompt_tokens: 1,
            completion_tokens: 3,
            finish_reason: "stop".to_owned(),
            tool_calls: Vec::new(),
        })
    }
    async fn knowledge_search(
        &self,
        _scope: &TenantScope,
        _request: KnowledgeSearchRequest,
    ) -> Result<KnowledgeSearchResult, HostOpError> {
        unreachable!()
    }
    async fn manifest_read(
        &self,
        _scope: &TenantScope,
        _request: ManifestReadRequest,
    ) -> Result<ManifestPage, HostOpError> {
        unreachable!()
    }
    async fn publish_submit(
        &self,
        _scope: &TenantScope,
        _request: PublishRequest,
    ) -> Result<PublishReceipt, HostOpError> {
        unreachable!()
    }
    async fn measure_sample(
        &self,
        _scope: &TenantScope,
        _request: MeasureRequest,
    ) -> Result<MeasureSample, HostOpError> {
        unreachable!()
    }
}

fn bundle() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/agent-runtime/dist/memeloop-content-workflow.bundle.mjs");
    fs::read_to_string(path).expect("generate the approved bundle with pnpm agent:bundle")
}

#[test]
fn content_host_requests_reject_scope_bodies_and_policy_overrides() {
    let id = Uuid::new_v4();
    for field in [
        "tenant_id",
        "body",
        "prompt",
        "model",
        "source_version_id",
        "url",
    ] {
        let mut request = serde_json::json!({
            "execution_id": id, "item_id": Uuid::new_v4()
        });
        request[field] = serde_json::json!("not-authorized");
        assert!(
            serde_json::from_value::<ContentStepRequest>(request).is_err(),
            "{field} must not cross to JavaScript"
        );
    }
    assert!(
        serde_json::from_value::<ContentItemsReadRequest>(serde_json::json!({
            "execution_id": id, "project_id": Uuid::new_v4()
        }))
        .is_err()
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires the generated memeloop@0.3.3 content bundle"]
async fn blocked_branch_keeps_the_denominator_and_does_not_prevent_other_preparation() {
    let fixture = Arc::new(Fixture::new());
    fixture.items.lock().unwrap()[1].status = ContentItemStatus::Blocked;
    *fixture.interrupt_once.lock().unwrap() = false;
    let scope = TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    );
    let source = bundle();
    let modules = [(ENTRY, source.as_str())];
    let input = serde_json::json!({ "execution_id": fixture.execution_id }).to_string();
    let mut runtime = HostRuntime::new(
        &modules,
        HostBridge::new(fixture.clone(), scope, tokio::runtime::Handle::current()),
        Some(64 * 1024 * 1024),
    )
    .unwrap();
    runtime.call_main(ENTRY, &input, DEADLINE).await.unwrap();
    assert_eq!(
        fixture.generated.lock().unwrap().as_slice(),
        &[Uuid::from_u128(1)]
    );
    assert_eq!(
        fixture.items.lock().unwrap()[1].status,
        ContentItemStatus::Blocked
    );
    assert!(
        runtime
            .host_state()
            .events
            .iter()
            .any(|event| event.topic == "distribution.prepared")
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires the generated memeloop@0.3.3 content bundle"]
async fn two_documents_resume_after_generation_without_duplicate_model_call() {
    let fixture = Arc::new(Fixture::new());
    let scope = TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    );
    let source = bundle();
    let modules = [(ENTRY, source.as_str())];
    let input = serde_json::json!({ "execution_id": fixture.execution_id }).to_string();

    let mut first = HostRuntime::new(
        &modules,
        HostBridge::new(
            fixture.clone(),
            scope.clone(),
            tokio::runtime::Handle::current(),
        ),
        Some(64 * 1024 * 1024),
    )
    .unwrap();
    assert!(
        first.call_main(ENTRY, &input, DEADLINE).await.is_err(),
        "first handoff must remain incomplete"
    );
    assert_eq!(fixture.generated.lock().unwrap().len(), 2);
    drop(first);
    let mut resumed = HostRuntime::new(
        &modules,
        HostBridge::new(fixture.clone(), scope, tokio::runtime::Handle::current()),
        Some(64 * 1024 * 1024),
    )
    .unwrap();
    resumed.call_main(ENTRY, &input, DEADLINE).await.unwrap();
    assert_eq!(
        fixture.generated.lock().unwrap().len(),
        2,
        "durable drafts must not be regenerated"
    );
    assert!(
        resumed
            .host_state()
            .events
            .iter()
            .any(|event| event.topic == "distribution.prepared")
    );
    assert_eq!(*fixture.expansion.lock().unwrap(), 300);
    assert_eq!(fixture.items.lock().unwrap()[0].automatic_repair_count, 2);
    assert!(
        fixture.resume_pages.lock().unwrap().contains(&Some(255)),
        "eligibility recheck must reach target pages beyond the first 256"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires the generated memeloop@0.3.3 content bundle"]
async fn closed_execution_resumes_frozen_distribution_without_regenerating_or_sending() {
    let fixture = Arc::new(Fixture::new());
    for item in fixture.items.lock().unwrap().iter_mut() {
        item.status = ContentItemStatus::Ready;
    }
    *fixture.interrupt_expansion_once.lock().unwrap() = true;
    let scope = TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    );
    let source = bundle();
    let modules = [(ENTRY, source.as_str())];
    let input = serde_json::json!({ "execution_id": fixture.execution_id }).to_string();
    let mut first = HostRuntime::new(
        &modules,
        HostBridge::new(
            fixture.clone(),
            scope.clone(),
            tokio::runtime::Handle::current(),
        ),
        Some(64 * 1024 * 1024),
    )
    .unwrap();
    assert!(first.call_main(ENTRY, &input, DEADLINE).await.is_err());
    assert_eq!(*fixture.expansion.lock().unwrap(), 128);
    assert_eq!(fixture.generated.lock().unwrap().len(), 0);
    drop(first);
    let mut resumed = HostRuntime::new(
        &modules,
        HostBridge::new(fixture.clone(), scope, tokio::runtime::Handle::current()),
        Some(64 * 1024 * 1024),
    )
    .unwrap();
    resumed.call_main(ENTRY, &input, DEADLINE).await.unwrap();
    assert_eq!(fixture.generated.lock().unwrap().len(), 0);
    assert!(
        resumed
            .host_state()
            .events
            .iter()
            .any(|event| event.topic == "distribution.prepared")
    );
    assert!(fixture.resume_pages.lock().unwrap().contains(&Some(255)));
}
