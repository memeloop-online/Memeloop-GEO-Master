//! The real generated MemeLoop bundle drives the Rust-owned channel tools.
//! The model fixture selects references; it cannot submit body or credentials.

use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use chrono::Utc;
use geo_api::{AppState, EmbeddedAgentRuntime, ModelProviderBridge, RepositoryHostOps};
use geo_domain::{
    AgentRuntime, ChannelAccount, ChannelAccountRecord, ChannelOwnerKind, ChannelStatus,
    DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, ImportItem, KnowledgePurpose, ProjectCreate,
    ProjectSettings, ProjectStartCommand, SourceKind, TenantScope, TurnInput, hash_idempotency_key,
    settings_hash, start_request_hash,
};
use geo_worker::{
    HostOpError, ModelCompletion, ModelCompletionRequest, ModelToolCall, ModelToolFunctionCall,
};
use serde_json::{Value, json};
use uuid::Uuid;

const BUNDLE_SPECIFIER: &str = "memeloop://bundle/memeloop-agent-loop.bundle.mjs";

#[derive(Default)]
struct ChannelModel {
    calls: Mutex<usize>,
    plan_ids: Mutex<Vec<String>>,
}

fn call(name: &str, argument: Value, index: usize) -> ModelToolCall {
    ModelToolCall {
        id: format!("channel-call-{index}"),
        kind: "function".into(),
        function: ModelToolFunctionCall {
            name: name.into(),
            arguments: argument.to_string(),
        },
    }
}

fn last_result(request: &ModelCompletionRequest) -> Value {
    let tool = request
        .messages
        .iter()
        .rev()
        .find(|message| message.role == "tool")
        .expect("actual tool result reaches next model call");
    serde_json::from_str(tool.content.as_deref().unwrap()).unwrap()
}

#[async_trait]
impl ModelProviderBridge for ChannelModel {
    async fn complete(
        &self,
        _scope: &TenantScope,
        request: &ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError> {
        for name in [
            "channel_discover",
            "channel_plan",
            "channel_manifest_read",
            "channel_target_execute",
        ] {
            assert!(request.tools.iter().any(|tool| tool.function.name == name));
        }
        let mut phase = self.calls.lock().unwrap();
        let step = *phase % 6;
        *phase += 1;
        let mut result = ModelCompletion {
            text: String::new(),
            tool_calls: vec![],
            model: "fixture-model".into(),
            prompt_tokens: 5,
            completion_tokens: 5,
            finish_reason: "tool_calls".into(),
        };
        let tool = match step {
            0 => call("channel_discover", json!({"kind":"public_sources"}), *phase),
            1 => {
                let source = last_result(request);
                assert_eq!(source["kind"], "public_sources");
                assert_eq!(source["items"].as_array().unwrap().len(), 1);
                assert_eq!(source["items"][0]["media_type"], "text/plain");
                call("channel_discover", json!({"kind":"accounts"}), *phase)
            }
            2 => {
                let account = last_result(request);
                assert_eq!(account["kind"], "accounts");
                assert_eq!(account["items"].as_array().unwrap().len(), 1);
                let source = request
                    .messages
                    .iter()
                    .filter(|message| message.role == "tool")
                    .find_map(|message| {
                        let value: Value =
                            serde_json::from_str(message.content.as_deref()?).ok()?;
                        (value["kind"] == "public_sources").then_some(value)
                    })
                    .unwrap();
                call(
                    "channel_plan",
                    json!({
                        "publications":[{
                            "source_id":source["items"][0]["source_id"],
                            "source_version_id":source["items"][0]["source_version_id"],
                            "platform":account["items"][0]["platform"],
                            "account_id":account["items"][0]["account_id"]
                        }],
                        "measurements":[]
                    }),
                    *phase,
                )
            }
            3 => {
                let plan = last_result(request);
                assert_eq!(plan["expected_count"], 1);
                assert_eq!(plan["dispatch_state"], "pending");
                self.plan_ids
                    .lock()
                    .unwrap()
                    .push(plan["plan_id"].as_str().unwrap().to_owned());
                call(
                    "channel_manifest_read",
                    json!({"cycle_id":plan["cycle_id"]}),
                    *phase,
                )
            }
            4 => {
                let manifest = last_result(request);
                assert_eq!(manifest["sealed"], true);
                assert_eq!(manifest["items"].as_array().unwrap().len(), 1);
                assert!(manifest["items"][0].get("body").is_none());
                call(
                    "channel_target_execute",
                    json!({"target_id":manifest["items"][0]["target_id"]}),
                    *phase,
                )
            }
            _ => {
                let execution = last_result(request);
                assert_eq!(execution["state"], "deferred");
                assert_eq!(execution["deferred_reason"], "runner_unavailable");
                result.text = "A target is planned, but runner execution is deferred.".into();
                result.finish_reason = "stop".into();
                return Ok(result);
            }
        };
        result.tool_calls.push(tool);
        Ok(result)
    }
}

async fn fixture() -> (AppState, TenantScope) {
    let state = AppState::development_with_password("local-fixture");
    let base = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let project = state
        .project_repository()
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
    state
        .project_repository()
        .start(
            &base,
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("channel-runtime-fixture"),
                request_hash: start_request_hash(project.id, project.revision, &settings_hash),
                settings_hash,
                operation_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    state
        .knowledge_repository()
        .import_batch(
            &scope,
            vec![ImportItem {
                client_item_id: "public".into(),
                kind: SourceKind::Text,
                name: "Approved source".into(),
                purpose: KnowledgePurpose::Public,
                text: Some("A fixture source suitable for publication.".into()),
                url: None,
                object_id: None,
                knowledge_release_id: None,
            }],
        )
        .await
        .unwrap();
    state
        .channel_service()
        .repository
        .save_account(
            &scope,
            ChannelAccountRecord {
                account: ChannelAccount {
                    account_id: Uuid::new_v4(),
                    project_id: project.id,
                    owner_kind: ChannelOwnerKind::Customer,
                    platform: "zhihu".into(),
                    group_id: None,
                    status: ChannelStatus::NeedsLogin,
                    display_name: None,
                    platform_account_id: None,
                    avatar_url: None,
                    enabled: true,
                    proxy_configured: false,
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
    (state, scope)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires `pnpm agent:bundle`; generated ESM is intentionally not tracked"]
async fn generated_memeloop_discovers_plans_reads_and_defers_without_external_send() {
    let (state, scope) = fixture().await;
    let model = Arc::new(ChannelModel::default());
    let ops = RepositoryHostOps::new(state.knowledge_repository())
        .with_channels(state.clone())
        .with_model_provider(model.clone());
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/agent-runtime/dist/memeloop-agent-loop.bundle.mjs");
    let source: &'static str = Box::leak(
        fs::read_to_string(path)
            .expect("run pnpm agent:bundle first")
            .into_boxed_str(),
    );
    let modules: &'static [(&str, &str)] = Box::leak(Box::new([(BUNDLE_SPECIFIER, source)]));
    let runtime = EmbeddedAgentRuntime::with_bundle(modules, BUNDLE_SPECIFIER, Arc::new(ops));
    for _ in 0..2 {
        let report = runtime
            .run_turn(
                &scope,
                TurnInput {
                    conversation_id: Uuid::new_v4().into(),
                    message_id: Uuid::new_v4().into(),
                    turn_id: Uuid::new_v4().into(),
                    run_id: Uuid::new_v4().into(),
                    prompt: "Plan one publication from the available public source.".into(),
                    attachments: vec![],
                },
            )
            .await
            .unwrap();
        assert!(report.content.contains("deferred"));
    }
    assert_eq!(*model.calls.lock().unwrap(), 12);
    let plan_ids = model.plan_ids.lock().unwrap();
    assert_eq!(plan_ids.len(), 2);
    assert_eq!(plan_ids[0], plan_ids[1], "frozen plan must replay");
}
