use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use geo_worker::{
    ChannelDiscoverRequest, ChannelDiscoveryItem, ChannelDiscoveryKind, ChannelDiscoveryPage,
    ChannelExecutionResult, ChannelExecutionState, ChannelManifestPage, ChannelManifestReadRequest,
    ChannelPlanReceipt, ChannelPlanRequest, ChannelPublicationLookupObservation,
    ChannelPublicationLookupSummary, ChannelTargetExecuteRequest, ChannelTargetKind,
    ChannelTargetSummary, DistributionTargetRef, HostBridge, HostOp, HostOpError, HostOps,
    HostRuntime, KnowledgeSearchRequest, KnowledgeSearchResult, ManifestPage, ManifestReadRequest,
    MeasureRequest, MeasureSample, ModelCompletion, ModelCompletionRequest, PublishReceipt,
    PublishRequest, TenantScope,
};
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Default)]
struct Missing;

#[async_trait]
impl HostOps for Missing {
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

struct Available;

#[async_trait]
impl HostOps for Available {
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
    async fn channel_discover(
        &self,
        _: &TenantScope,
        request: ChannelDiscoverRequest,
    ) -> Result<ChannelDiscoveryPage, HostOpError> {
        Ok(ChannelDiscoveryPage {
            kind: request.kind,
            current_cycle_id: Some(Uuid::new_v4()),
            items: vec![ChannelDiscoveryItem::PublicSource {
                source_id: Uuid::new_v4(),
                source_version_id: Uuid::new_v4(),
                name: "example".into(),
                media_type: "text/plain".into(),
            }],
            next_cursor: None,
        })
    }
    async fn channel_plan(
        &self,
        _: &TenantScope,
        request: ChannelPlanRequest,
    ) -> Result<ChannelPlanReceipt, HostOpError> {
        Ok(ChannelPlanReceipt {
            plan_id: Uuid::new_v4(),
            cycle_id: request.cycle_id.unwrap_or_else(Uuid::new_v4),
            revision: 1,
            expected_count: (request.publications.len() + request.measurements.len()) as u64,
            dispatch_state: "pending".into(),
        })
    }
    async fn channel_manifest_read(
        &self,
        _: &TenantScope,
        request: ChannelManifestReadRequest,
    ) -> Result<ChannelManifestPage, HostOpError> {
        let target_id = Uuid::new_v4();
        Ok(ChannelManifestPage {
            plan_id: Uuid::new_v4(),
            cycle_id: request.cycle_id.unwrap_or_else(Uuid::new_v4),
            revision: request.revision.unwrap_or(1),
            sealed: true,
            expected_count: 1,
            items: vec![ChannelTargetSummary {
                target_id,
                kind: ChannelTargetKind::Publish,
                account_id: Uuid::new_v4(),
                platform_or_provider: "example-platform".into(),
                source_id: Some(Uuid::new_v4()),
                source_version_id: Some(Uuid::new_v4()),
                scheduled_at: None,
                execution: ChannelExecutionResult {
                    target_id,
                    state: ChannelExecutionState::Pending,
                    attempt_id: None,
                    outcome_status: None,
                    deferred_reason: None,
                    public_url: None,
                    evidence_ref: None,
                    fixture: None,
                },
                publication_lookup: None,
            }],
            next_cursor: None,
        })
    }
    async fn channel_target_execute(
        &self,
        _: &TenantScope,
        request: ChannelTargetExecuteRequest,
    ) -> Result<ChannelExecutionResult, HostOpError> {
        Ok(ChannelExecutionResult {
            target_id: request.target_id,
            state: ChannelExecutionState::Deferred,
            attempt_id: None,
            outcome_status: None,
            deferred_reason: Some("unavailable".into()),
            public_url: None,
            evidence_ref: None,
            fixture: None,
        })
    }
}

fn event(runtime: &HostRuntime, topic: &str) -> Value {
    let state = runtime.host_state();
    let event = state
        .events
        .iter()
        .find(|event| event.topic == topic)
        .unwrap();
    serde_json::from_str(&event.payload).unwrap()
}

#[tokio::test]
async fn four_channel_bridges_delegate_and_unconfigured_capabilities_fail_closed() {
    let source_id = Uuid::new_v4();
    let source_version_id = Uuid::new_v4();
    let account_id = Uuid::new_v4();
    let target_id = Uuid::new_v4();
    let script = format!(
        r#"
        import {{ hostOps, attempt }} from "./host-ops.js";
        await attempt("discover", () => hostOps.channelDiscover({{kind:"public_sources"}}));
        await attempt("plan", () => hostOps.channelPlan({{
          publications:[{{source_id:"{source_id}",source_version_id:"{source_version_id}",platform:"example-platform",account_id:"{account_id}"}}],
          measurements:[]
        }}));
        await attempt("manifest", () => hostOps.channelManifestRead({{}}));
        await attempt("execute", () => hostOps.channelTargetExecute({{target_id:"{target_id}"}}));
    "#
    );
    let bundle = [
        ("memeloop://bundle/host-ops.js", geo_worker::HOST_OPS_JS),
        ("memeloop://bundle/channel-scenario.js", script.as_str()),
    ];
    for (capabilities, configured) in [
        (Arc::new(Missing) as Arc<dyn HostOps>, false),
        (Arc::new(Available) as Arc<dyn HostOps>, true),
    ] {
        let scope = TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        );
        let bridge = HostBridge::new(capabilities, scope, tokio::runtime::Handle::current());
        let mut runtime = HostRuntime::new(&bundle, bridge, None).unwrap();
        runtime
            .evaluate_module(
                "memeloop://bundle/channel-scenario.js",
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert_eq!(runtime.host_ops_version(), geo_worker::HOST_OPS_VERSION);
        for (topic, op) in [
            ("discover", HostOp::ChannelDiscover),
            ("plan", HostOp::ChannelPlan),
            ("manifest", HostOp::ChannelManifestRead),
            ("execute", HostOp::ChannelTargetExecute),
        ] {
            let record = event(&runtime, topic);
            assert_eq!(record["ok"], configured, "{record}");
            if configured {
                assert!(record["value"].is_object());
            } else {
                assert_eq!(record["name"], "GeoHostOpError");
                assert_eq!(record["error"]["code"], "capability_missing");
                assert_eq!(record["error"]["op"], serde_json::to_value(op).unwrap());
            }
            assert_eq!(runtime.op_calls(op), 1);
        }
    }
}

#[tokio::test]
async fn requests_reject_scope_fields_nested_extensions_and_bad_ids_before_capability() {
    let source_id = Uuid::new_v4();
    let source_version_id = Uuid::new_v4();
    let account_id = Uuid::new_v4();
    let script = format!(
        r#"
        import {{ hostOps, attempt }} from "./host-ops.js";
        await attempt("discover-scope", () => hostOps.channelDiscover({{kind:"public_sources",tenant_id:"foreign"}}));
        await attempt("discover-limit", () => hostOps.channelDiscover({{kind:"accounts",limit:101}}));
        await attempt("plan-nested", () => hostOps.channelPlan({{
          publications:[{{source_id:"{source_id}",source_version_id:"{source_version_id}",platform:"example-platform",account_id:"{account_id}",body:"forbidden"}}],
          measurements:[]
        }}));
        await attempt("plan-scope", () => hostOps.channelPlan({{project_id:"foreign",publications:[],measurements:[]}}));
        await attempt("manifest-scope", () => hostOps.channelManifestRead({{project_id:"foreign"}}));
        await attempt("manifest-limit", () => hostOps.channelManifestRead({{limit:101}}));
        await attempt("execute-nil", () => hostOps.channelTargetExecute({{target_id:"00000000-0000-0000-0000-000000000000"}}));
        await attempt("execute-body", () => hostOps.channelTargetExecute({{target_id:"{source_id}",body:"forbidden"}}));
    "#
    );
    let bundle = [
        ("memeloop://bundle/host-ops.js", geo_worker::HOST_OPS_JS),
        ("memeloop://bundle/rejection.js", script.as_str()),
    ];
    let scope = TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    );
    let bridge = HostBridge::new(Arc::new(Missing), scope, tokio::runtime::Handle::current());
    let mut runtime = HostRuntime::new(&bundle, bridge, None).unwrap();
    runtime
        .evaluate_module("memeloop://bundle/rejection.js", Duration::from_secs(10))
        .await
        .unwrap();
    for topic in [
        "discover-scope",
        "discover-limit",
        "plan-nested",
        "plan-scope",
        "manifest-scope",
        "manifest-limit",
        "execute-nil",
        "execute-body",
    ] {
        assert_eq!(
            event(&runtime, topic)["error"]["code"],
            "invalid_request",
            "{topic}"
        );
    }
    for op in [
        HostOp::ChannelDiscover,
        HostOp::ChannelPlan,
        HostOp::ChannelManifestRead,
        HostOp::ChannelTargetExecute,
    ] {
        assert_eq!(runtime.op_calls(op), 0);
    }
}

#[test]
fn channel_dtos_are_strict_and_keep_old_publish_contract() {
    assert!(
        serde_json::from_value::<ChannelDiscoverRequest>(
            json!({"kind":"accounts","operator_id":Uuid::new_v4()})
        )
        .is_err()
    );
    assert!(serde_json::from_value::<ChannelPlanRequest>(json!({"publications":[],"measurements":[{"account_id":Uuid::new_v4(),"provider":"one","model":"one","surface":"consumer_web","search_mode":"official","protocol_version":"1","question_set_version":"1","question":"hello","market":"en","language":"en","scheduled_at":"2026-10-03T00:00:00Z","sample_ordinal":0,"credential":"forbidden"}]})).is_err());
    assert!(
        serde_json::from_value::<ChannelTargetExecuteRequest>(
            json!({"target_id":Uuid::new_v4(),"publication_intent_id":Uuid::new_v4()})
        )
        .is_err()
    );
    assert!(serde_json::from_value::<PublishRequest>(json!({"target_id":Uuid::new_v4()})).is_err());
    assert_eq!(
        ChannelDiscoveryKind::PublicSources,
        serde_json::from_str("\"public_sources\"").unwrap()
    );
}

#[test]
fn manifest_lookup_is_optional_bounded_and_cannot_upgrade_the_original_attempt() {
    let target_id = Uuid::new_v4();
    let request = ChannelManifestReadRequest {
        cycle_id: None,
        revision: None,
        cursor: None,
        limit: None,
    };
    let old = json!({
        "plan_id":Uuid::new_v4(),"cycle_id":Uuid::new_v4(),
        "revision":1,"sealed":true,"expected_count":1,
        "items":[{
            "target_id":target_id,"kind":"publish","account_id":Uuid::new_v4(),
            "platform_or_provider":"example",
            "source_id":Uuid::new_v4(),"source_version_id":Uuid::new_v4(),
            "execution":{"target_id":target_id,"state":"unknown_result","attempt_id":Uuid::new_v4()}
        }]
    });
    let mut page: ChannelManifestPage = serde_json::from_value(old).unwrap();
    assert!(page.validate_for(&request).is_ok());
    assert!(page.items[0].publication_lookup.is_none());
    page.items[0].publication_lookup = Some(ChannelPublicationLookupSummary {
        query_count: 2,
        next_due_at: None,
        in_progress: false,
        last_error_code: Some("lookup_error".into()),
        latest_observation: Some(ChannelPublicationLookupObservation {
            finding: geo_domain::PublicationLookupFinding::AssetObserved,
            observed_at: chrono::DateTime::parse_from_rfc3339("2026-10-06T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            received_at: chrono::DateTime::parse_from_rfc3339("2026-10-06T00:00:01Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        }),
    });
    assert!(page.validate_for(&request).is_ok());
    let encoded = serde_json::to_value(&page).unwrap();
    assert_eq!(encoded["items"][0]["execution"]["state"], "unknown_result");
    assert_eq!(
        encoded["items"][0]["publication_lookup"]["latest_observation"]["finding"],
        "asset_observed"
    );
    assert!(
        encoded["items"][0]["publication_lookup"]
            .get("public_url")
            .is_none()
    );
    let decoded: ChannelManifestPage = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, page);

    let mut invalid = page.clone();
    invalid.items[0].execution.state = ChannelExecutionState::Completed;
    assert!(invalid.validate_for(&request).is_err());
    let mut invalid = page.clone();
    invalid.items[0]
        .publication_lookup
        .as_mut()
        .unwrap()
        .last_error_code = Some("untrusted connector response".into());
    assert!(invalid.validate_for(&request).is_err());
    let mut invalid = serde_json::to_value(page).unwrap();
    invalid["items"][0]["publication_lookup"]["latest_observation"]["evidence"] = json!("private");
    assert!(serde_json::from_value::<ChannelManifestPage>(invalid).is_err());
}

#[test]
fn distribution_target_lookup_is_optional_and_rejects_raw_evidence() {
    let original = Uuid::new_v4();
    let mut value = json!({
        "target_id": Uuid::new_v4(),
        "ordinal": 1,
        "document_item_id": Uuid::new_v4(),
        "content_revision_id": Uuid::new_v4(),
        "platform_id": "generic",
        "variant_id": Uuid::new_v4(),
        "publication_intent_id": Uuid::new_v4(),
        "status": "reused_unknown",
        "reason": null,
        "original_channel_target_id": original,
        "publication_lookup": {
            "query_count": 1,
            "in_progress": false,
            "last_error_code": "lookup_error",
            "latest_observation": {
                "finding": "asset_observed",
                "observed_at": "2026-10-06T00:00:00Z",
                "received_at": "2026-10-06T00:00:01Z"
            }
        }
    });
    let target: DistributionTargetRef = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(target.original_channel_target_id, Some(original));
    assert_eq!(target.publication_lookup.as_ref().unwrap().query_count, 1);
    let decoded: DistributionTargetRef =
        serde_json::from_value(serde_json::to_value(&target).unwrap()).unwrap();
    assert_eq!(decoded, target);
    value["publication_lookup"]["latest_observation"]["evidence"] = json!({"private": true});
    assert!(serde_json::from_value::<DistributionTargetRef>(value).is_err());
    let mut old = serde_json::to_value(target).unwrap();
    old.as_object_mut()
        .unwrap()
        .remove("original_channel_target_id");
    old.as_object_mut().unwrap().remove("publication_lookup");
    let old: DistributionTargetRef = serde_json::from_value(old).unwrap();
    assert!(old.original_channel_target_id.is_none());
    assert!(old.publication_lookup.is_none());
}
