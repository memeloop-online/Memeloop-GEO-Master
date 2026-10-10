use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use geo_worker::{
    HostBridge, HostOp, HostOpBudgets, HostOpError, HostOpErrorCode, HostOpLimits, HostOps,
    KnowledgeSearchRequest, KnowledgeSearchResult, ManifestPage, ManifestReadRequest,
    MeasureRequest, MeasureSample, ModelCompletion, ModelCompletionRequest, PublishReceipt,
    PublishRequest, PublishState, TenantScope, ToolCallIdentity, ToolCallOutcome, ToolCallRecorder,
};
use uuid::Uuid;

struct NoCapabilities;
#[async_trait]
impl HostOps for NoCapabilities {
    async fn model_complete(
        &self,
        _: &TenantScope,
        _: ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError> {
        unreachable!()
    }
    async fn knowledge_search(
        &self,
        _: &TenantScope,
        _: KnowledgeSearchRequest,
    ) -> Result<KnowledgeSearchResult, HostOpError> {
        unreachable!()
    }
    async fn manifest_read(
        &self,
        _: &TenantScope,
        _: ManifestReadRequest,
    ) -> Result<ManifestPage, HostOpError> {
        unreachable!()
    }
    async fn publish_submit(
        &self,
        _: &TenantScope,
        _: PublishRequest,
    ) -> Result<PublishReceipt, HostOpError> {
        unreachable!()
    }
    async fn measure_sample(
        &self,
        _: &TenantScope,
        _: MeasureRequest,
    ) -> Result<MeasureSample, HostOpError> {
        unreachable!()
    }
}

#[derive(Default)]
struct Recorder {
    seen: Mutex<Vec<(String, String, Option<ToolCallOutcome>)>>,
    fail_begin: AtomicBool,
    reject_attempt: AtomicBool,
    fail_finish: AtomicBool,
    cancel_after_attempt: Mutex<Option<Arc<AtomicBool>>>,
}

#[async_trait]
impl ToolCallRecorder for Recorder {
    async fn begin(&self, identity: &ToolCallIdentity) -> Result<bool, HostOpError> {
        self.seen
            .lock()
            .unwrap()
            .push(("begin".into(), identity.arguments_hash.clone(), None));
        if self.fail_begin.load(Ordering::SeqCst) {
            Err(HostOpError::internal(
                HostOp::Publish,
                "intent write failed",
            ))
        } else {
            Ok(true)
        }
    }

    async fn attempt(&self, identity: &ToolCallIdentity) -> Result<bool, HostOpError> {
        self.seen
            .lock()
            .unwrap()
            .push(("attempt".into(), identity.arguments_hash.clone(), None));
        if let Some(flag) = self.cancel_after_attempt.lock().unwrap().as_ref() {
            flag.store(true, Ordering::SeqCst);
        }
        Ok(!self.reject_attempt.load(Ordering::SeqCst))
    }

    async fn finish(
        &self,
        identity: &ToolCallIdentity,
        outcome: ToolCallOutcome,
    ) -> Result<(), HostOpError> {
        self.seen.lock().unwrap().push((
            "finish".into(),
            identity.arguments_hash.clone(),
            Some(outcome),
        ));
        if self.fail_finish.load(Ordering::SeqCst) {
            Err(HostOpError::internal(
                HostOp::Publish,
                "outcome write failed",
            ))
        } else {
            Ok(())
        }
    }
}

fn bridge(recorder: Arc<Recorder>, timeout_ms: u64) -> HostBridge {
    HostBridge::new(
        Arc::new(NoCapabilities),
        TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        ),
        tokio::runtime::Handle::current(),
    )
    .with_budgets(
        HostOpBudgets::default().with_limits(HostOp::Publish, HostOpLimits::new(timeout_ms, 8)),
    )
    .with_recorder(geo_domain::RunId::from(Uuid::new_v4()), recorder)
}

fn request() -> PublishRequest {
    let body = "synthetic confidential publication text".to_owned();
    PublishRequest {
        publication_intent_id: Uuid::new_v4(),
        document_revision_id: Uuid::new_v4(),
        platform_target_id: Uuid::new_v4(),
        payload_sha256: geo_domain::sha256_hex(body.as_bytes()),
        body,
    }
}

fn receipt(state: PublishState) -> PublishReceipt {
    PublishReceipt {
        publish_attempt_id: Uuid::new_v4(),
        state,
        external_url: None,
        evidence_ref: Some(Uuid::new_v4()),
    }
}

#[tokio::test]
async fn durable_order_and_secret_free_digest() {
    let recorder = Arc::new(Recorder::default());
    let bridge = bridge(Arc::clone(&recorder), 1000);
    let request = request();
    let called = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&called);
    let result = bridge
        .invoke_recorded(HostOp::Publish, &request, move |_| async move {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(receipt(PublishState::Published))
        })
        .await
        .unwrap();
    assert_eq!(result.state, PublishState::Published);
    assert_eq!(called.load(Ordering::SeqCst), 1);
    let events = recorder.seen.lock().unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event.0.as_str())
            .collect::<Vec<_>>(),
        ["begin", "attempt", "finish"]
    );
    assert_eq!(events[2].2, Some(ToolCallOutcome::Succeeded));
    assert!(events.iter().all(|event| {
        event.1.len() == 64 && !event.1.contains(&request.body) && event.1 == events[0].1
    }));
}

#[tokio::test]
async fn no_dispatch_on_intent_error_or_duplicate_attempt() {
    for (fail_begin, reject_attempt, expected_count) in [(true, false, 1), (false, true, 2)] {
        let recorder = Arc::new(Recorder::default());
        recorder.fail_begin.store(fail_begin, Ordering::SeqCst);
        recorder
            .reject_attempt
            .store(reject_attempt, Ordering::SeqCst);
        let bridge = bridge(Arc::clone(&recorder), 1000);
        let called = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&called);
        assert!(
            bridge
                .invoke_recorded(HostOp::Publish, &request(), move |_| async move {
                    counted.fetch_add(1, Ordering::SeqCst);
                    Ok(receipt(PublishState::Published))
                })
                .await
                .is_err()
        );
        assert_eq!(called.load(Ordering::SeqCst), 0);
        assert_eq!(recorder.seen.lock().unwrap().len(), expected_count);
    }
}

#[tokio::test]
async fn unknown_return_invalid_receipt_and_timeout_are_not_success() {
    let recorder = Arc::new(Recorder::default());
    let bridge = bridge(Arc::clone(&recorder), 500);
    let result = bridge
        .invoke_recorded(HostOp::Publish, &request(), |_| async move {
            Ok(receipt(PublishState::UnknownResult))
        })
        .await
        .unwrap();
    assert_eq!(result.state, PublishState::UnknownResult);
    assert_eq!(
        recorder.seen.lock().unwrap().last().unwrap().2,
        Some(ToolCallOutcome::Unknown)
    );

    let invalid = bridge
        .invoke_recorded(HostOp::Publish, &request(), |_| async move {
            Ok(PublishReceipt {
                publish_attempt_id: Uuid::nil(),
                state: PublishState::Published,
                evidence_ref: None,
                external_url: None,
            })
        })
        .await
        .unwrap_err();
    assert_eq!(invalid.code, HostOpErrorCode::UnknownResult);
    assert_eq!(
        recorder.seen.lock().unwrap().last().unwrap().2,
        Some(ToolCallOutcome::Unknown)
    );

    let recorder = Arc::new(Recorder::default());
    let timeout_bridge = self::bridge(Arc::clone(&recorder), 100);
    assert!(
        timeout_bridge
            .invoke_recorded(HostOp::Publish, &request(), |_| async move {
                tokio::time::sleep(Duration::from_millis(500)).await;
                Ok(receipt(PublishState::Published))
            })
            .await
            .is_err()
    );
    assert_eq!(
        recorder.seen.lock().unwrap().last().unwrap().2,
        Some(ToolCallOutcome::Unknown)
    );
}

#[tokio::test]
async fn dropping_isolate_wait_does_not_drop_application_finalization() {
    let recorder = Arc::new(Recorder::default());
    let bridge = bridge(Arc::clone(&recorder), 1000);
    let caller = tokio::spawn(async move {
        bridge
            .invoke_recorded(HostOp::Publish, &request(), |_| async move {
                tokio::time::sleep(Duration::from_millis(60)).await;
                Ok(receipt(PublishState::Published))
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if recorder.seen.lock().unwrap().len() >= 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("attempt should be claimed");
    caller.abort();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if recorder
                .seen
                .lock()
                .unwrap()
                .last()
                .is_some_and(|event| event.2 == Some(ToolCallOutcome::Succeeded))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("detached application task must finalize its outcome");
}

#[tokio::test]
async fn failing_terminal_write_never_returns_false_success() {
    let recorder = Arc::new(Recorder::default());
    recorder.fail_finish.store(true, Ordering::SeqCst);
    let bridge = bridge(Arc::clone(&recorder), 1000);
    let error = bridge
        .invoke_recorded(HostOp::Publish, &request(), |_| async move {
            Ok(receipt(PublishState::Published))
        })
        .await
        .unwrap_err();
    assert_eq!(error.code, HostOpErrorCode::Internal);
    assert_eq!(
        recorder.seen.lock().unwrap().last().unwrap().2,
        Some(ToolCallOutcome::Succeeded)
    );
}

#[tokio::test]
async fn cancellation_after_attempt_claim_prevents_dispatch_and_records_unknown() {
    let recorder = Arc::new(Recorder::default());
    let bridge = bridge(Arc::clone(&recorder), 1000);
    *recorder.cancel_after_attempt.lock().unwrap() = Some(bridge.cancellation());
    let called = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&called);
    let error = bridge
        .invoke_recorded(HostOp::Publish, &request(), move |_| async move {
            observed.store(true, Ordering::SeqCst);
            Ok(receipt(PublishState::Published))
        })
        .await
        .unwrap_err();
    assert_eq!(error.code, HostOpErrorCode::UnknownResult);
    assert!(!called.load(Ordering::SeqCst));
    assert_eq!(
        recorder.seen.lock().unwrap().last().unwrap().2,
        Some(ToolCallOutcome::Unknown)
    );
}

#[tokio::test]
async fn panic_after_attempt_is_recorded_unknown() {
    let recorder = Arc::new(Recorder::default());
    let bridge = bridge(Arc::clone(&recorder), 1000);
    let error = bridge
        .invoke_recorded(HostOp::Publish, &request(), |_| async move {
            panic!("synthetic capability panic");
            #[allow(unreachable_code)]
            Ok(receipt(PublishState::Published))
        })
        .await
        .unwrap_err();
    assert_eq!(error.code, HostOpErrorCode::UnknownResult);
    assert_eq!(
        recorder.seen.lock().unwrap().last().unwrap().2,
        Some(ToolCallOutcome::Unknown)
    );
}
