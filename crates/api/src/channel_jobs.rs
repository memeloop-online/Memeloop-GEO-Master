//! Channel planning/dispatch. Only source versions and frozen questions are
//! accepted publicly; outcomes are exclusively written from the runner.

use axum::{
    Json,
    extract::{Extension, Path, State},
};
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, AuthorizePublicationSend, ChannelOutcome, ChannelOutcomeStatus, ChannelPlan,
    ChannelStatus, ChannelTarget, ChannelTargetInput, ChannelTargetView, ConnectorAvailability,
    ConnectorKey, ErrorCode, KnowledgePurpose, ProjectId, ProjectStatus, PublicationOrigin,
    QuestionReference, RICH_DISTRIBUTION_FORMAT, RICH_MARKDOWN_FORMAT, SourceState, TenantScope,
    sha256_hex, validate_rich_publication_payload,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{ApiError, AppState, AuthContext, RequestContext, api_error, require_project_writer};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanRequest {
    pub publications: Vec<PublicationRequest>,
    pub measurements: Vec<MeasurementRequest>,
    #[serde(default)]
    pub bound_measurements: Vec<BoundMeasurementRequest>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationRequest {
    pub source_id: Uuid,
    pub source_version_id: Uuid,
    pub platform: String,
    pub account_id: Uuid,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MeasurementRequest {
    pub account_id: Uuid,
    pub provider: String,
    pub model: String,
    pub surface: String,
    pub search_mode: String,
    pub protocol_version: String,
    pub question_set_version: String,
    pub question: String,
    pub market: String,
    pub language: String,
    pub scheduled_at: chrono::DateTime<Utc>,
    pub sample_ordinal: u32,
}

/// No caller-controlled question text, market, language, purpose or split.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BoundMeasurementRequest {
    pub account_id: Uuid,
    pub provider: String,
    pub model: String,
    pub surface: String,
    pub search_mode: String,
    pub protocol_version: String,
    pub question: QuestionReference,
    pub scheduled_at: DateTime<Utc>,
    pub sample_ordinal: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteRequest {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelDispatchDeferred {
    ProjectInactive,
    ScheduledForLater,
    AccountUnavailable,
    RunnerUnavailable,
    AccountBusy,
    SourceUnavailable,
    ConnectorUnavailable,
    FixtureOnly,
}

/// A callback is an independently configured service capability, not a
/// connector's plain publish proof or a claim made by the runner itself.
fn rich_send_bridge_available(state: &AppState) -> bool {
    state.publication_send_callback.is_some() && state.channel_service().browser.is_some()
}

fn rich_runner_operation_available(
    connectors: &[crate::browser_bridge::RunnerConnector],
    key: &ConnectorKey,
    version: &str,
) -> bool {
    connectors
        .iter()
        .filter(|connector| {
            connector.platform == key.platform_id
                && connector.placement_slot == key.placement_slot
                && connector.connector_version == version
                && connector
                    .operations
                    .iter()
                    .any(|operation| operation == "rich_publish")
        })
        .count()
        == 1
}

fn rich_send_required(input: &ChannelTargetInput) -> bool {
    matches!(
        input,
        ChannelTargetInput::GeneratedPublish {
            rich_payload: Some(_),
            ..
        }
    )
}

async fn generated_connector_available(
    state: &AppState,
    scope: &TenantScope,
    input: &ChannelTargetInput,
) -> Result<bool, AppError> {
    let ChannelTargetInput::GeneratedPublish {
        platform,
        publication_intent_id,
        distribution_target_id,
        origin_request_id,
        rich_payload,
        ..
    } = input
    else {
        return Ok(true);
    };
    // The target retains its immutable snapshot; it cannot be silently
    // upgraded after freeze. Current operator revocation/version drift may
    // still prevent a new attempt against that snapshot.
    let bundle = state
        .distribution_repository()
        .get_publication_bundle(scope, *publication_intent_id)
        .await?;
    bundle.validate_origin()?;
    let frozen = match bundle.origin {
        PublicationOrigin::ContentRequest { request } => {
            if Some(request.request_id) != *origin_request_id
                || !distribution_target_id.is_nil()
                || request.platform_id != *platform
                || request.format
                    != if rich_payload.is_some() {
                        RICH_DISTRIBUTION_FORMAT
                    } else {
                        geo_domain::TEXT_DISTRIBUTION_FORMAT
                    }
            {
                return Ok(false);
            }
            let key = ConnectorKey {
                platform_id: platform.clone(),
                placement_slot: request.placement_slot,
            };
            let connectors = crate::connector_capabilities::deployed_versions(state).await;
            let Some(version) = crate::connector_capabilities::deployed_version(&connectors, &key)
            else {
                return Ok(false);
            };
            if rich_payload.is_some()
                && !rich_runner_operation_available(&connectors, &key, version)
            {
                return Ok(false);
            }
            let Some(settings) = state
                .connector_capability_repository()
                .get(scope.operator_id, &key)
                .await?
            else {
                return Ok(false);
            };
            if !settings.enabled {
                return Ok(false);
            }
            let content = state.content_repository();
            let semantic =
                if let Some(asset) = content.get_asset(scope, request.content_asset_id).await? {
                    content
                        .get_item(scope, asset.execution_id, asset.item_id)
                        .await?
                        .map(|item| item.content_type)
                } else {
                    None
                };
            let proof_format = if rich_payload.is_some() {
                crate::connector_capabilities::configured_rich_publication_format(&settings)
            } else if settings
                .content_types
                .iter()
                .any(|format| format == geo_domain::PLAIN_TEXT_ARTICLE_FORMAT)
            {
                Some(geo_domain::PLAIN_TEXT_ARTICLE_FORMAT)
            } else {
                semantic.as_deref().filter(|semantic| {
                    geo_domain::publication_format_for_semantic_type(semantic).is_some()
                        && settings.content_types.iter().any(|item| item == *semantic)
                })
            };
            let Some(proof_format) = proof_format else {
                return Ok(false);
            };
            return Ok(state
                .connector_capability_repository()
                .resolve(scope.operator_id, &key, version, proof_format)
                .await?
                .availability
                == ConnectorAvailability::Available);
        }
        PublicationOrigin::CoverageTarget { target } => {
            if origin_request_id.is_some() {
                return Ok(false);
            }
            target
        }
    };
    let manifest = state
        .distribution_repository()
        .get(scope, frozen.manifest_id)
        .await?;
    let Some(document) = manifest
        .document_roster
        .iter()
        .find(|item| item.document_item_id == frozen.document_item_id)
    else {
        return Ok(false);
    };
    let Some(placement) = manifest.platform_scope.iter().find(|item| {
        item.platform_id == frozen.platform_id && item.placement_slot == frozen.placement_slot
    }) else {
        return Ok(false);
    };
    let key = ConnectorKey {
        platform_id: platform.clone(),
        placement_slot: frozen.placement_slot.clone(),
    };
    let connectors = crate::connector_capabilities::deployed_versions(state).await;
    let Some(version) = crate::connector_capabilities::deployed_version(&connectors, &key) else {
        return Ok(false);
    };
    if rich_payload.is_some() && !rich_runner_operation_available(&connectors, &key, version) {
        return Ok(false);
    }
    if frozen.target_id != *distribution_target_id
        || frozen.platform_id != *platform
        || placement.capability_version != version
        || placement.fixture
        || placement.unavailable_reason.is_some()
        || !placement
            .supported_formats
            .contains(&if rich_payload.is_some() {
                RICH_MARKDOWN_FORMAT.to_owned()
            } else {
                document.content_type.clone()
            })
    {
        return Ok(false);
    }
    let settings = state
        .connector_capability_repository()
        .get(scope.operator_id, &key)
        .await?;
    let Some(settings) = settings else {
        return Ok(false);
    };
    let proof_format = if rich_payload.is_some() {
        crate::connector_capabilities::configured_rich_publication_format(&settings)
    } else {
        crate::connector_capabilities::configured_publication_format(
            &settings,
            &document.content_type,
        )
    };
    let Some(proof_format) = proof_format else {
        return Ok(false);
    };
    if state
        .connector_capability_repository()
        .resolve(scope.operator_id, &key, version, proof_format)
        .await?
        .availability
        == ConnectorAvailability::Available
    {
        return Ok(true);
    }
    Ok(false)
}

async fn legacy_connector_available(
    state: &AppState,
    scope: &TenantScope,
    input: &ChannelTargetInput,
) -> Result<bool, AppError> {
    let ChannelTargetInput::Publish { platform, .. } = input else {
        return Ok(true);
    };
    let key = ConnectorKey {
        platform_id: platform.clone(),
        placement_slot: "primary".into(),
    };
    let Some(settings) = state
        .connector_capability_repository()
        .get(scope.operator_id, &key)
        .await?
    else {
        // Legacy source publication is the trusted bootstrap probe: absence
        // of a registry entry cannot depend on an already verified proof.
        return Ok(true);
    };
    if !settings.enabled {
        return Ok(false);
    }
    let connectors = crate::connector_capabilities::deployed_versions(state).await;
    let Some(version) = crate::connector_capabilities::deployed_version(&connectors, &key) else {
        return Ok(false);
    };
    // Source publications have no document semantic type. They can bootstrap
    // an absent configuration, but configured sends require proof of their
    // actual title/body wire representation, not any unrelated semantic proof.
    let Some(format) = crate::connector_capabilities::configured_source_format(&settings) else {
        return Ok(false);
    };
    Ok(state
        .connector_capability_repository()
        .resolve(scope.operator_id, &key, version, format)
        .await?
        .availability
        == ConnectorAvailability::Available)
}

#[derive(Debug)]
pub enum ChannelDispatchResult {
    Executed(Box<ChannelTargetView>),
    Deferred(ChannelDispatchDeferred),
}

fn error(error: AppError, context: RequestContext) -> ApiError {
    api_error(error, context.request_id)
}

pub(crate) async fn scope(
    state: &AppState,
    tenant: &TenantScope,
    project_id: ProjectId,
) -> Result<TenantScope, AppError> {
    state
        .project_repository()
        .get(tenant, project_id)
        .await?
        .ok_or_else(|| AppError::not_found("project not found"))?;
    Ok(TenantScope::new(
        tenant.operator_id,
        tenant.tenant_id,
        Some(project_id),
    ))
}

fn bounded(value: &str, name: &str, limit: usize) -> Result<(), AppError> {
    if value.trim().is_empty() || value.len() > limit {
        Err(AppError::invalid_request(format!(
            "{name} is missing or too long"
        )))
    } else {
        Ok(())
    }
}

fn target_id(cycle: Uuid, input: &ChannelTargetInput) -> Result<Uuid, AppError> {
    let mut hash = Sha256::new();
    hash.update(cycle.as_bytes());
    hash.update(
        serde_json::to_vec(input)
            .map_err(|_| AppError::invalid_request("invalid channel target"))?,
    );
    let bytes: [u8; 16] = hash.finalize()[..16].try_into().expect("sha256 length");
    Ok(Uuid::from_bytes(bytes))
}

fn request_hash(targets: &[ChannelTarget]) -> Result<String, AppError> {
    Ok(sha256_hex(&serde_json::to_vec(targets).map_err(|_| {
        AppError::invalid_request("invalid channel plan")
    })?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicationCandidate {
    kind: String,
    schema_version: String,
    url: String,
    expected_sha256: String,
    observed_at: DateTime<Utc>,
    source: String,
}

/// Retain only a bounded, canonical post-submit hint, not a verified receipt.
/// Association identifiers are supplied by Rust, never by adapter JSON.
fn publication_candidate(
    value: &serde_json::Value,
    target: &ChannelTarget,
    attempt_id: Uuid,
    claimed_at: DateTime<Utc>,
    received_at: DateTime<Utc>,
    connector_version: &str,
) -> Option<serde_json::Value> {
    // A title/body hint says nothing about a structured tree or its images.
    if matches!(
        &target.input,
        ChannelTargetInput::GeneratedPublish {
            rich_payload: Some(_),
            ..
        }
    ) {
        return None;
    }
    let hint: PublicationCandidate = serde_json::from_value(value.clone()).ok()?;
    let (platform, title, body) = match &target.input {
        ChannelTargetInput::Publish {
            platform,
            title,
            body,
            ..
        }
        | ChannelTargetInput::GeneratedPublish {
            platform,
            title,
            body,
            ..
        } => (platform, title, body),
        ChannelTargetInput::Measure { .. } => return None,
    };
    let normalize = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let expected = sha256_hex(format!("{}\n{}", normalize(title), normalize(body)).as_bytes());
    if hint.kind != "publication_candidate"
        || hint.schema_version != "geo.publication.candidate.v1"
        || hint.source != "post_submit_navigation"
        || hint.expected_sha256 != expected
        || hint.observed_at < claimed_at
        || hint.observed_at > received_at
        || hint.url.len() > 256
        || platform != "zhihu"
    {
        return None;
    }
    let url = reqwest::Url::parse(&hint.url).ok()?;
    let post_id = url.path().strip_prefix("/p/")?;
    if url.scheme() != "https"
        || !matches!(url.host_str(), Some("www.zhihu.com" | "zhuanlan.zhihu.com"))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.as_str() != hint.url
        || post_id.is_empty()
        || !post_id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some(json!({
        "kind": "publication_candidate",
        "schema_version": "geo.publication.candidate.v1",
        "url": hint.url,
        "expected_sha256": expected,
        "observed_at": hint.observed_at,
        "source": hint.source,
        "attempt_id": attempt_id,
        "target_id": target.target_id,
        "account_id": target.input.account_id(),
        "connector_version": connector_version,
    }))
}

fn retain_publication_candidate(
    result: &mut crate::browser_bridge::BrowserExecution,
    target: &ChannelTarget,
    attempt: &geo_domain::ChannelAttempt,
    received_at: DateTime<Utc>,
    fixture: bool,
    status: ChannelOutcomeStatus,
) {
    let candidates: Vec<_> = result
        .evidence
        .iter()
        .filter(|proof| proof["kind"] == "publication_candidate")
        .collect();
    let candidate = if !fixture && status == ChannelOutcomeStatus::Unknown && candidates.len() == 1
    {
        publication_candidate(
            candidates[0],
            target,
            attempt.attempt_id,
            attempt.claimed_at,
            received_at,
            result.connector_version.as_deref().unwrap_or_default(),
        )
    } else {
        None
    };
    result
        .evidence
        .retain(|proof| proof["kind"] != "publication_candidate");
    if let Some(candidate) = candidate {
        result.evidence.push(candidate);
    }
}

pub(crate) fn publication_readback(
    result: &crate::browser_bridge::BrowserExecution,
    target: &ChannelTargetInput,
) -> bool {
    // Until the runner supplies a separately typed structured/media readback,
    // the legacy title/body hash must never verify a rich send or lookup.
    if matches!(
        target,
        ChannelTargetInput::GeneratedPublish {
            rich_payload: Some(_),
            ..
        }
    ) {
        return false;
    }
    if result.provenance != Some(crate::browser_bridge::BrowserReceiptProvenance::Live)
        || result.connector_version.as_deref().is_none_or(|version| {
            version.trim().is_empty() || version.len() > 100 || version.starts_with("fixture")
        })
        || result
            .evidence
            .iter()
            .any(|proof| proof["kind"] == "runner_receipt")
    {
        return false;
    }
    let (platform, title, body) = match target {
        ChannelTargetInput::Publish {
            platform,
            title,
            body,
            ..
        }
        | ChannelTargetInput::GeneratedPublish {
            platform,
            title,
            body,
            ..
        } => (platform, title, body),
        ChannelTargetInput::Measure { .. } => return false,
    };
    let normalize = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let expected_hash = sha256_hex(format!("{}\n{}", normalize(title), normalize(body)).as_bytes());
    if result.status != "completed"
        || result.stage.as_deref() != Some("public_readback")
        || result.occurred_at.is_none()
    {
        return false;
    }
    let Some(url) = result
        .public_url
        .as_ref()
        .and_then(|url| reqwest::Url::parse(url).ok())
    else {
        return false;
    };
    let post_id = url
        .path()
        .strip_prefix("/p/")
        .unwrap_or_default()
        .trim_end_matches('/');
    if url.scheme() != "https"
        || !matches!(url.host_str(), Some("www.zhihu.com" | "zhuanlan.zhihu.com"))
        || platform != "zhihu"
        || post_id.is_empty()
        || !post_id.bytes().all(|byte| byte.is_ascii_digit())
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    result.evidence.iter().any(|proof| {
        proof.get("kind").and_then(|v| v.as_str()) == Some("public_readback")
            && proof.get("url").and_then(|v| v.as_str()) == Some(url.as_str())
            && proof.get("content_matched").and_then(|v| v.as_bool()) == Some(true)
            && proof.get("owned_by_account").and_then(|v| v.as_bool()) == Some(true)
            && proof.get("expected_sha256").and_then(|v| v.as_str())
                == proof.get("readback_sha256").and_then(|v| v.as_str())
            && proof.get("expected_sha256").and_then(|v| v.as_str()) == Some(expected_hash.as_str())
    })
}

// The authenticated runner is the only source of these fields. A plausible
// answer, citations, or a caller-supplied search_verified flag is not proof.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OfficialSearchEvent {
    kind: String,
    event_id: String,
    request_id: String,
    occurred_at: DateTime<Utc>,
    source: String,
    provenance: String,
}

/// Connect providers may expose chat/message/block identities without a
/// provider request ID. Preserve their actual correlation fields instead of
/// manufacturing the v1 request_id/event_id pair.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectSearchEvent {
    kind: String,
    source: String,
    provenance: String,
    chat_id: String,
    message_id: String,
    block_id: String,
    event_offset: String,
    /// Browser-local observation time, not a claimed provider timestamp.
    observed_at: DateTime<Utc>,
    request_model: String,
    request_question_sha256: String,
}

/// Semantic extraction from the authenticated runner, not a deterministic
/// decoding of provider fields. Raw-source grounding remains runner-owned.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AiConnectSearchEvent {
    kind: String,
    source: String,
    provenance: String,
    chat_id: String,
    message_id: String,
    block_id: String,
    observed_at: DateTime<Utc>,
    request_model: String,
    request_question_sha256: String,
    extraction_model: String,
    extraction_prompt_version: String,
    source_sha256: String,
}

/// Browser-response interpretation is independent of provider protocol IDs.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AiBrowserSearchEvent {
    kind: String,
    source: String,
    provenance: String,
    search_used: SearchUsed,
    observed_at: DateTime<Utc>,
    request_model: String,
    request_question_sha256: String,
    extraction_model: String,
    extraction_prompt_version: String,
    source_sha256: String,
}

#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum SearchUsed {
    Yes,
    No,
    Unknown,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SearchEvent {
    Provider(OfficialSearchEvent),
    Connect(ConnectSearchEvent),
    AiConnect(AiConnectSearchEvent),
    AiBrowser(AiBrowserSearchEvent),
}

fn extraction_label(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 200 && !value.chars().any(char::is_control)
}

fn extraction_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn search_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

impl SearchEvent {
    fn observed_at(
        &self,
        schema: &str,
        model: &str,
        question_sha256: &str,
    ) -> Option<DateTime<Utc>> {
        match self {
            Self::Provider(event)
                if schema == "geo.measure.official_search.v1"
                    && event.kind == "official_search_event"
                    && event.source == "provider_search_event"
                    && event.provenance == "live"
                    && search_identifier(&event.event_id)
                    && search_identifier(&event.request_id) =>
            {
                Some(event.occurred_at)
            }
            Self::Connect(event)
                if schema == "geo.measure.official_search.v2"
                    && event.kind == "official_search_event"
                    && event.source == "provider_connect_stream"
                    && event.provenance == "live"
                    && search_identifier(&event.chat_id)
                    && search_identifier(&event.message_id)
                    && search_identifier(&event.block_id)
                    && event.event_offset.len() <= 20
                    && event
                        .event_offset
                        .parse::<u64>()
                        .is_ok_and(|offset| offset.to_string() == event.event_offset)
                    && event.request_model == model
                    && event.request_question_sha256 == question_sha256 =>
            {
                Some(event.observed_at)
            }
            Self::AiConnect(event)
                if schema == "geo.measure.official_search.v3"
                    && event.kind == "official_search_event"
                    && event.source == "provider_connect_stream_ai"
                    && event.provenance == "live"
                    && search_identifier(&event.chat_id)
                    && search_identifier(&event.message_id)
                    && search_identifier(&event.block_id)
                    && event.request_model == model
                    && extraction_label(&event.request_model)
                    && event.request_question_sha256 == question_sha256
                    && extraction_sha256(&event.request_question_sha256)
                    && extraction_label(&event.extraction_model)
                    && extraction_label(&event.extraction_prompt_version)
                    && extraction_sha256(&event.source_sha256) =>
            {
                Some(event.observed_at)
            }
            Self::AiBrowser(event)
                if schema == "geo.measure.official_search.v4"
                    && event.kind == "official_search_event"
                    && event.source == "browser_response_ai"
                    && event.provenance == "live"
                    && event.search_used == SearchUsed::Yes
                    && event.request_model == model
                    && extraction_label(&event.request_model)
                    && event.request_question_sha256 == question_sha256
                    && extraction_sha256(&event.request_question_sha256)
                    && extraction_label(&event.extraction_model)
                    && event.extraction_prompt_version == "geo.observation.extract.v2"
                    && extraction_sha256(&event.source_sha256) =>
            {
                Some(event.observed_at)
            }
            _ => None,
        }
    }

    fn extraction_audit_binding(&self) -> Option<(&str, &str, &str)> {
        match self {
            Self::AiConnect(event) => Some((
                &event.extraction_model,
                &event.extraction_prompt_version,
                &event.source_sha256,
            )),
            Self::AiBrowser(event) => Some((
                &event.extraction_model,
                &event.extraction_prompt_version,
                &event.source_sha256,
            )),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MeasurementEvidence {
    kind: String,
    schema_version: String,
    target_id: Uuid,
    account_id: Uuid,
    provider: String,
    model: String,
    surface: String,
    search_mode: String,
    protocol_version: String,
    question_set_version: String,
    question_sha256: String,
    market: String,
    language: String,
    scheduled_at: DateTime<Utc>,
    sample_ordinal: u32,
    connector_version: String,
    provenance: String,
    disposition: String,
    raw_answer: String,
    citations: Vec<String>,
    search_event: SearchEvent,
}

pub(crate) fn measurement_observation(
    result: &crate::browser_bridge::BrowserExecution,
    target: &ChannelTarget,
    claimed_at: DateTime<Utc>,
    received_at: DateTime<Utc>,
) -> Option<(ChannelOutcomeStatus, String, Vec<String>)> {
    if result.provenance != Some(crate::browser_bridge::BrowserReceiptProvenance::Live)
        || result
            .evidence
            .iter()
            .any(|proof| proof["kind"] == "runner_receipt")
    {
        return None;
    }
    let ChannelTargetInput::Measure {
        account_id,
        provider,
        model,
        surface,
        search_mode,
        protocol_version,
        question_set_version,
        question,
        market,
        language,
        scheduled_at,
        sample_ordinal,
        ..
    } = &target.input
    else {
        return None;
    };
    if result.status != "completed"
        || result.stage.as_deref() != Some("official_search_observation")
        || result.public_url.is_some()
    {
        return None;
    }
    let mut proofs = result.evidence.iter().filter(|value| {
        value.get("kind").and_then(|kind| kind.as_str()) == Some("official_search_observation")
    });
    let proof: MeasurementEvidence = serde_json::from_value(proofs.next()?.clone()).ok()?;
    if proofs.next().is_some() {
        return None;
    }
    let version = result.connector_version.as_deref()?;
    if version.trim().is_empty()
        || version.len() > 100
        // Provenance is established by the typed proof, not substrings such
        // as "test" (which also occur in legitimate "attested" versions).
        || version.starts_with("fixture")
        || result.occurred_at.is_none()
        || proof.kind != "official_search_observation"
        || proof.target_id != target.target_id
        || proof.account_id != *account_id
        || proof.provider != *provider
        || proof.model != *model
        || proof.surface != *surface
        || proof.search_mode != *search_mode
        || proof.protocol_version != *protocol_version
        || proof.question_set_version != *question_set_version
        || proof.question_sha256 != sha256_hex(question.as_bytes())
        || proof.market != *market
        || proof.language != *language
        || proof.scheduled_at != *scheduled_at
        || proof.sample_ordinal != *sample_ordinal
        || proof.connector_version != version
        || proof.provenance != "live"
        || proof.raw_answer.trim().is_empty()
        || proof.raw_answer.len() > 100_000
        || proof.citations.len() > 50
    {
        return None;
    }
    let completed_at = result.occurred_at?;
    let observed_at =
        proof
            .search_event
            .observed_at(&proof.schema_version, model, &proof.question_sha256)?;
    if matches!(
        proof.schema_version.as_str(),
        "geo.measure.official_search.v2"
            | "geo.measure.official_search.v3"
            | "geo.measure.official_search.v4"
    ) && (surface != "consumer_web" || search_mode != "web_search")
    {
        return None;
    }
    if let Some((extraction_model, prompt_version, source_sha256)) =
        proof.search_event.extraction_audit_binding()
    {
        let mut audits = result
            .evidence
            .iter()
            .filter(|value| value["kind"] == "observation_extraction");
        let audit = audits.next()?;
        // Sanitized runner input is retained only in tenant-scoped evidence,
        // allowing replay without promoting model interpretation to raw facts.
        let source_json = audit["source_json"].as_str()?;
        if source_json.len() > 750_000 || sha256_hex(source_json.as_bytes()) != source_sha256 {
            return None;
        }
        let source: serde_json::Value = serde_json::from_str(source_json).ok()?;
        if !source.is_object() || !source["messages"].is_array() {
            return None;
        }
        if audits.next().is_some()
            || audit["method"] != "llm_grounded"
            || audit["model"].as_str() != Some(extraction_model)
            || audit["prompt_version"].as_str() != Some(prompt_version)
            || audit["source_sha256"].as_str() != Some(source_sha256)
            // This is the extractor's surface. API-based interpretation of
            // captured browser bytes does not change the measured surface.
            || !matches!(
                audit["surface"].as_str(),
                Some("signed_in_browser" | "model_api")
            )
            || !audit["refs"].as_array().is_some_and(|refs| !refs.is_empty())
        {
            return None;
        }
    }
    if claimed_at > observed_at
        || observed_at > completed_at
        || completed_at > received_at
        || completed_at < claimed_at
    {
        return None;
    }
    let status = match proof.disposition.as_str() {
        "observed" => ChannelOutcomeStatus::Observed,
        "refused" if proof.citations.is_empty() => ChannelOutcomeStatus::Refused,
        _ => return None,
    };
    for citation in &proof.citations {
        let url = reqwest::Url::parse(citation).ok()?;
        if citation.len() > 2048
            || !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return None;
        }
    }
    Some((status, proof.raw_answer, proof.citations))
}

/// The queued input is an index, never an authority for publishable bytes.
/// Return only after matching the immutable distribution chain and checking
/// every cited source's *current* publicly eligible version.
async fn generated_publication_preflight(
    state: &AppState,
    scope: &TenantScope,
    input: &ChannelTargetInput,
) -> Result<bool, AppError> {
    let ChannelTargetInput::GeneratedPublish {
        content_revision_id,
        variant_id,
        publication_intent_id,
        distribution_target_id,
        origin_request_id,
        platform,
        account_id,
        title,
        body,
        body_sha256,
        evidence,
        payload_hash,
        rich_payload,
    } = input
    else {
        return Ok(false);
    };
    let bundle = state
        .distribution_repository()
        .get_publication_bundle(scope, *publication_intent_id)
        .await?;
    let variant = &bundle.variant;
    let revision = &bundle.revision;
    let intent = &bundle.intent;
    let command = &bundle.command;
    bundle.validate_origin()?;
    if evidence.is_empty()
        || *content_revision_id != revision.revision_id
        || *variant_id != variant.variant_id
        || intent.intent_id != *publication_intent_id
        || intent.project_id != scope.project_id.expect("project scope checked")
        || intent.variant_id != variant.variant_id
        || intent.content_revision_id != revision.revision_id
        || intent.platform_id != *platform
        || intent.account_id != *account_id
        || variant.platform_id != *platform
        || variant.content_revision_id != revision.revision_id
        || variant.title != *title
        || variant.markdown != *body
        || variant.evidence != *evidence
        || revision.evidence != *evidence
        || revision.document.title != *title
        || revision.markdown != *body
        || revision.markdown != revision.document.markdown()
        || sha256_hex(body.as_bytes()) != *body_sha256
        || variant.payload_hash != *payload_hash
        || variant.rich_payload.as_ref() != rich_payload.as_ref()
        || intent.payload_hash != *payload_hash
        || command.payload_hash != *payload_hash
        || command.intent_id != intent.intent_id
    {
        return Err(AppError::conflict(
            "generated publication differs from frozen distribution",
        ));
    }
    if rich_payload.is_some() {
        validate_rich_publication_payload(variant).map_err(|_| {
            AppError::conflict("generated rich publication payload differs from frozen variant")
        })?;
        if revision.document.schema_version != Some(2)
            || variant
                .rich_payload
                .as_ref()
                .is_none_or(|payload| payload.document != revision.document)
        {
            return Err(AppError::conflict(
                "generated rich publication revision differs from frozen variant",
            ));
        }
    } else if revision.document.schema_version == Some(2) || variant.rich_payload.is_some() {
        return Err(AppError::conflict(
            "generated publication format differs from frozen revision",
        ));
    }
    match &bundle.origin {
        PublicationOrigin::CoverageTarget { target } => {
            if origin_request_id.is_some()
                || *distribution_target_id != target.target_id
                || intent.channel_target_id != target.target_id
                || command.target_id != target.target_id
                || target.content_revision_id != Some(revision.revision_id)
                || target.variant_id != Some(variant.variant_id)
                || target.publication_intent_id != Some(intent.intent_id)
                || target.account_id != Some(*account_id)
                || target.platform_id != *platform
            {
                return Err(AppError::conflict("covered publication origin differs"));
            }
        }
        PublicationOrigin::ContentRequest { request } => {
            if !distribution_target_id.is_nil()
                || *origin_request_id != Some(request.request_id)
                || !intent.channel_target_id.is_nil()
                || !command.target_id.is_nil()
                || request.scope != *scope
                || request.content_revision_id != revision.revision_id
                || request.content_asset_id != revision.asset_id
                || request.account_id != *account_id
                || request.platform_id != *platform
                || request.placement_slot != variant.placement_slot
                || request.format
                    != if rich_payload.is_some() {
                        RICH_DISTRIBUTION_FORMAT
                    } else {
                        geo_domain::TEXT_DISTRIBUTION_FORMAT
                    }
                || variant.policy_version
                    != if rich_payload.is_some() {
                        geo_domain::RICH_CHANNEL_VARIANT_POLICY
                    } else {
                        geo_domain::CHANNEL_VARIANT_POLICY
                    }
                || revision.findings.iter().any(|finding| finding.blocking)
            {
                return Err(AppError::conflict("requested publication origin differs"));
            }
            let content = state.content_repository();
            let stored = content
                .get_revision(scope, request.content_asset_id, request.content_revision_id)
                .await?
                .ok_or_else(|| AppError::conflict("publication revision unavailable"))?;
            if stored != *revision
                || !content
                    .list_checks(scope, request.content_revision_id)
                    .await?
                    .iter()
                    .any(|check| {
                        check.revision_id == request.content_revision_id
                            && !check.findings.iter().any(|finding| finding.blocking)
                    })
            {
                return Err(AppError::conflict("publication check no longer valid"));
            }
        }
    }
    if command.fixture {
        return Ok(true);
    }
    let knowledge = state.knowledge_repository();
    let sources = knowledge.list_sources(scope).await?;
    for cited in evidence {
        let source = sources.iter().find(|source| {
            source.current_version_id == Some(cited.source_version_id)
                && source.state == SourceState::Active
                && source.purpose == KnowledgePurpose::Public
        });
        let Some(source) = source else {
            return Err(AppError::conflict("source no longer publicly eligible"));
        };
        if knowledge
            .get_source_version(scope, source.source_id, cited.source_version_id)
            .await?
            .is_none()
        {
            return Err(AppError::conflict("source version no longer available"));
        };
        if matches!(bundle.origin, PublicationOrigin::ContentRequest { .. }) {
            let detail = knowledge
                .get_source_detail(scope, source.source_id)
                .await?
                .ok_or_else(|| AppError::conflict("source unavailable"))?;
            let quote = revision
                .quotes
                .iter()
                .find(|quote| quote.reference == *cited)
                .ok_or_else(|| AppError::conflict("source quote unavailable"))?;
            if !detail.chunks.iter().any(|chunk| {
                Some(chunk.chunk_id) == cited.chunk_id
                    && chunk.source_version_id == cited.source_version_id
                    && chunk.locator == cited.locator
                    && if matches!(chunk.locator, geo_domain::ChunkLocator::Csv { .. }) {
                        chunk.text == quote.exact_quote
                    } else {
                        chunk
                            .text
                            .chars()
                            .take(geo_domain::CONTENT_EVIDENCE_MAX_QUOTE_CHARS)
                            .collect::<String>()
                            == quote.exact_quote
                    }
            }) {
                return Err(AppError::conflict("source evidence quote changed"));
            }
        }
    }
    Ok(false)
}

/// Always release the fresh browser context after a resumed account has
/// completed identity verification and its one typed operation. The runner's
/// idle reaper remains the fallback if this process dies mid-request.
#[cfg(test)]
pub(crate) async fn execute_and_close(
    bridge: &crate::browser_bridge::BrowserBridge,
    session: Uuid,
    expected_identity: Option<&str>,
    attempt_id: Uuid,
    operation: &str,
    payload: &serde_json::Value,
) -> Result<crate::browser_bridge::BrowserExecution, AppError> {
    execute_and_close_with_cleanup(
        bridge,
        session,
        expected_identity,
        attempt_id,
        operation,
        payload,
    )
    .await
    .0
}

pub(crate) async fn execute_and_close_with_cleanup(
    bridge: &crate::browser_bridge::BrowserBridge,
    session: Uuid,
    expected_identity: Option<&str>,
    attempt_id: Uuid,
    operation: &str,
    payload: &serde_json::Value,
) -> (
    Result<crate::browser_bridge::BrowserExecution, AppError>,
    bool,
) {
    execute_and_close_with_cleanup_and_ticket(
        bridge,
        session,
        expected_identity,
        attempt_id,
        operation,
        payload,
        ExecutionSessionContext::default(),
    )
    .await
}

#[derive(Default)]
struct ExecutionSessionContext<'a> {
    source_capture_ticket: Option<&'a str>,
    renewal: Option<(
        &'a crate::channels::ChannelService,
        &'a TenantScope,
        &'a mut geo_domain::ChannelSessionVersion,
    )>,
}

async fn retain_browser_renewal(
    bridge: &crate::browser_bridge::BrowserBridge,
    session: Uuid,
    service: &crate::channels::ChannelService,
    scope: &TenantScope,
    version: &mut geo_domain::ChannelSessionVersion,
) {
    // A renewal failure must not erase a result after an external send.
    let result = async {
        let verified = bridge.complete(session).await?;
        service
            .persist_browser_renewal(scope, version, &verified)
            .await
    }
    .await;
    if result.is_err() {
        tracing::warn!("browser session renewal could not be retained");
    }
}

async fn execute_and_close_with_cleanup_and_ticket(
    bridge: &crate::browser_bridge::BrowserBridge,
    session: Uuid,
    expected_identity: Option<&str>,
    attempt_id: Uuid,
    operation: &str,
    payload: &serde_json::Value,
    mut context: ExecutionSessionContext<'_>,
) -> (
    Result<crate::browser_bridge::BrowserExecution, AppError>,
    bool,
) {
    let result = async {
        let verified = bridge.complete(session).await?;
        if Some(verified.identity.platform_account_id.as_str()) != expected_identity {
            return Err(AppError::conflict("account identity changed"));
        }
        if let Some((service, scope, version)) = context.renewal.as_mut() {
            service
                .persist_browser_renewal(scope, version, &verified)
                .await?;
        }
        bridge
            .execute_with_source_capture_ticket(
                attempt_id,
                session,
                operation,
                payload,
                context.source_capture_ticket,
            )
            .await
    }
    .await;
    if let Some((service, scope, version)) = context.renewal.as_mut() {
        retain_browser_renewal(bridge, session, service, scope, version).await;
    }
    let closed = bridge.close(session).await.is_ok();
    if !closed {
        // A successful external result is still evidence if cleanup fails.
        // The runner owns a bounded idle reaper as the crash/outage fallback.
        tracing::warn!("browser execution session cleanup failed");
    }
    (result, closed)
}

/// Resolves the actual source text and verifies its current public eligibility
/// before freezing exact bytes, not a synthetic generated article.
async fn publication_input(
    state: &AppState,
    scope: &TenantScope,
    request: PublicationRequest,
) -> Result<ChannelTargetInput, AppError> {
    if !matches!(
        request.platform.as_str(),
        "zhihu" | "baidu_creator" | "xiaohongshu"
    ) {
        return Err(AppError::invalid_request(
            "unsupported publication platform",
        ));
    }
    let account = state
        .channel_service()
        .resolve_available_account(scope, request.account_id)
        .await?;
    if account.platform != request.platform {
        return Err(AppError::invalid_request(
            "account platform differs from target",
        ));
    }
    let detail = state
        .knowledge_repository()
        .get_source_detail(scope, request.source_id)
        .await?
        .ok_or_else(|| AppError::not_found("source not found"))?;
    if detail.source.purpose != KnowledgePurpose::Public
        || detail.source.state != SourceState::Active
    {
        return Err(AppError::conflict("source is not currently public"));
    }
    let version = detail
        .versions
        .iter()
        .find(|version| version.source_version_id == request.source_version_id)
        .ok_or_else(|| AppError::not_found("source version not found"))?;
    if version.project_id != scope.project_id.expect("checked scope") {
        return Err(AppError::forbidden("source version outside project"));
    }
    let body = detail
        .chunks
        .iter()
        .filter(|chunk| chunk.source_version_id == version.source_version_id)
        .map(|chunk| chunk.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    bounded(&body, "source body", 256 * 1024)?;
    bounded(&detail.source.name, "source title", 240)?;
    Ok(ChannelTargetInput::Publish {
        source_id: request.source_id,
        source_version_id: request.source_version_id,
        platform: request.platform,
        account_id: request.account_id,
        title: detail.source.name,
        body_sha256: sha256_hex(body.as_bytes()),
        body,
    })
}

pub(crate) async fn measurement_input(
    state: &AppState,
    scope: &TenantScope,
    request: MeasurementRequest,
) -> Result<ChannelTargetInput, AppError> {
    if request.provider != "kimi"
        || request.surface != "consumer_web"
        || !matches!(request.search_mode.as_str(), "web_search" | "standard")
    {
        return Err(AppError::invalid_request(
            "measurement requires an explicit supported consumer-web protocol",
        ));
    }
    let account = state
        .channel_service()
        .resolve_available_account(scope, request.account_id)
        .await?;
    if account.platform != "kimi" {
        return Err(AppError::invalid_request(
            "measurement account platform differs",
        ));
    }
    for (name, value, limit) in [
        ("model", &request.model, 100),
        ("protocol_version", &request.protocol_version, 100),
        ("question_set_version", &request.question_set_version, 100),
        ("question", &request.question, 4000),
        ("market", &request.market, 100),
        ("language", &request.language, 100),
    ] {
        bounded(value, name, limit)?;
    }
    if request.sample_ordinal > 10000 {
        return Err(AppError::invalid_request("sample ordinal exceeds limit"));
    }
    Ok(ChannelTargetInput::Measure {
        account_id: request.account_id,
        provider: request.provider,
        model: request.model,
        surface: request.surface,
        search_mode: request.search_mode,
        protocol_version: request.protocol_version,
        question_set_version: request.question_set_version,
        question: request.question,
        market: request.market,
        language: request.language,
        scheduled_at: request.scheduled_at,
        sample_ordinal: request.sample_ordinal,
        question_binding: None,
    })
}

pub(crate) async fn bound_measurement_input(
    state: &AppState,
    scope: &TenantScope,
    request: BoundMeasurementRequest,
) -> Result<ChannelTargetInput, AppError> {
    let resolved = state
        .question_repository()
        .resolve_question(scope, request.question)
        .await?;
    let mut input = measurement_input(
        state,
        scope,
        MeasurementRequest {
            account_id: request.account_id,
            provider: request.provider,
            model: request.model,
            surface: request.surface,
            search_mode: request.search_mode,
            protocol_version: request.protocol_version,
            question_set_version: resolved
                .binding
                .reference
                .question_set_version_id
                .to_string(),
            question: resolved.revision.text.clone(),
            market: resolved.revision.market.clone(),
            language: resolved.revision.language.clone(),
            scheduled_at: request.scheduled_at,
            sample_ordinal: request.sample_ordinal,
        },
    )
    .await?;
    if let ChannelTargetInput::Measure {
        question_binding, ..
    } = &mut input
    {
        *question_binding = Some(resolved.binding);
    }
    Ok(input)
}

pub async fn submit_plan(
    State(state): State<AppState>,
    Path((project_id, cycle_id)): Path<(ProjectId, Uuid)>,
    Extension(tenant): Extension<TenantScope>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(request): Json<PlanRequest>,
) -> Result<Json<ChannelPlan>, ApiError> {
    require_project_writer(&auth).map_err(|e| error(e, context))?;
    let scope = scope(&state, &tenant, project_id)
        .await
        .map_err(|e| error(e, context))?;
    create_channel_plan(&state, &scope, cycle_id, request)
        .await
        .map(Json)
        .map_err(|e| error(e, context))
}

/// Shared Rust-owned plan construction for the HTTP and Agent tool paths.
/// The caller supplies an already-authorized project scope; the plan is
/// idempotently frozen by the repository.
pub async fn create_channel_plan(
    state: &AppState,
    scope: &TenantScope,
    cycle_id: Uuid,
    request: PlanRequest,
) -> Result<ChannelPlan, AppError> {
    let project_id = scope
        .project_id
        .ok_or_else(|| AppError::forbidden("project scope required"))?;
    if state
        .project_repository()
        .get_report_cycle(scope, project_id, cycle_id)
        .await?
        .is_none()
    {
        return Err(AppError::not_found("cycle not found"));
    }
    if request.publications.len() + request.measurements.len() + request.bound_measurements.len()
        > 100
    {
        return Err(AppError::invalid_request(
            "channel plan exceeds 100 targets",
        ));
    }
    let mut targets = Vec::new();
    for publish in request.publications {
        let input = publication_input(state, scope, publish).await?;
        targets.push(ChannelTarget {
            target_id: target_id(cycle_id, &input)?,
            input,
        });
    }
    for measure in request.measurements {
        let input = measurement_input(state, scope, measure).await?;
        targets.push(ChannelTarget {
            target_id: target_id(cycle_id, &input)?,
            input,
        });
    }
    for measure in request.bound_measurements {
        let input = bound_measurement_input(state, scope, measure).await?;
        targets.push(ChannelTarget {
            target_id: target_id(cycle_id, &input)?,
            input,
        });
    }
    let mut ids = std::collections::HashSet::new();
    if !targets.iter().all(|target| ids.insert(target.target_id)) {
        return Err(AppError::invalid_request("duplicate channel target"));
    }
    let plan = ChannelPlan {
        plan_id: Uuid::new_v4(),
        project_id,
        cycle_id,
        input_hash: request_hash(&targets)?,
        revision: 1,
        created_at: Utc::now(),
        targets,
    };
    state
        .channel_job_repository()
        .create_plan(scope, plan)
        .await
}

pub async fn get_plan(
    State(state): State<AppState>,
    Path((project_id, cycle_id)): Path<(ProjectId, Uuid)>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<ChannelPlan>, ApiError> {
    let scope = scope(&state, &tenant, project_id)
        .await
        .map_err(|e| error(e, context))?;
    let plan = state
        .channel_job_repository()
        .get_plan(&scope, cycle_id)
        .await
        .map_err(|e| error(e, context))?
        .ok_or_else(|| error(AppError::not_found("channel plan not found"), context))?;
    Ok(Json(plan))
}

pub async fn get_target(
    State(state): State<AppState>,
    Path((project_id, target_id)): Path<(ProjectId, Uuid)>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<ChannelTargetView>, ApiError> {
    let scope = scope(&state, &tenant, project_id)
        .await
        .map_err(|e| error(e, context))?;
    state
        .channel_job_repository()
        .get_target(&scope, target_id)
        .await
        .map(Json)
        .map_err(|e| error(e, context))
}

pub async fn execute_target(
    State(state): State<AppState>,
    Path((project_id, target_id)): Path<(ProjectId, Uuid)>,
    Extension(tenant): Extension<TenantScope>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(_request): Json<ExecuteRequest>,
) -> Result<Json<ChannelTargetView>, ApiError> {
    require_project_writer(&auth).map_err(|e| error(e, context))?;
    let scope = scope(&state, &tenant, project_id)
        .await
        .map_err(|e| error(e, context))?;
    match execute_channel_target(&state, &scope, target_id)
        .await
        .map_err(|e| error(e, context))?
    {
        ChannelDispatchResult::Executed(view) => Ok(Json(*view)),
        ChannelDispatchResult::Deferred(reason) => Err(error(
            AppError::conflict(format!("channel target deferred: {reason:?}")),
            context,
        )),
    }
}

/// Accepts only a trusted project scope (from authorization or repository
/// discovery), never an untrusted project selector from a queued payload.
/// Inexpensive and reversible preflight happens before the one-shot claim.
/// Once claimed, a crash leaves the target unknown for reconciliation.
pub async fn execute_channel_target(
    state: &AppState,
    scope: &TenantScope,
    target_id: Uuid,
) -> Result<ChannelDispatchResult, AppError> {
    let project_id = scope
        .project_id
        .ok_or_else(|| AppError::forbidden("project scope required"))?;
    let project = state
        .project_repository()
        .get(scope, project_id)
        .await?
        .ok_or_else(|| AppError::not_found("project not found"))?;
    if matches!(
        project.status,
        ProjectStatus::Paused | ProjectStatus::Archived
    ) {
        return Ok(ChannelDispatchResult::Deferred(
            ChannelDispatchDeferred::ProjectInactive,
        ));
    }
    let repo = state.channel_job_repository();
    let planned = repo.get_target(scope, target_id).await?;
    if !planned.attempts.is_empty() {
        return Err(AppError::conflict(
            "target already attempted; inspect or reconcile existing outcome",
        ));
    }
    if !legacy_connector_available(state, scope, &planned.target.input).await? {
        return Ok(ChannelDispatchResult::Deferred(
            ChannelDispatchDeferred::ConnectorUnavailable,
        ));
    }
    if matches!(
        &planned.target.input,
        ChannelTargetInput::GeneratedPublish { .. }
    ) {
        match generated_publication_preflight(state, scope, &planned.target.input).await {
            Ok(true) => {
                return Ok(ChannelDispatchResult::Deferred(
                    ChannelDispatchDeferred::FixtureOnly,
                ));
            }
            Ok(false) => {}
            Err(error) if error.message.starts_with("source ") => {
                return Ok(ChannelDispatchResult::Deferred(
                    ChannelDispatchDeferred::SourceUnavailable,
                ));
            }
            Err(error) => return Err(error),
        }
        if rich_send_required(&planned.target.input) && !rich_send_bridge_available(state) {
            return Ok(ChannelDispatchResult::Deferred(
                ChannelDispatchDeferred::ConnectorUnavailable,
            ));
        }
        if !generated_connector_available(state, scope, &planned.target.input).await? {
            return Ok(ChannelDispatchResult::Deferred(
                ChannelDispatchDeferred::ConnectorUnavailable,
            ));
        }
    }
    if let ChannelTargetInput::Measure { scheduled_at, .. } = &planned.target.input
        && *scheduled_at > Utc::now()
    {
        return Ok(ChannelDispatchResult::Deferred(
            ChannelDispatchDeferred::ScheduledForLater,
        ));
    }
    let service = state.channel_service();
    let Some(bridge) = service.browser.as_ref() else {
        return Ok(ChannelDispatchResult::Deferred(
            ChannelDispatchDeferred::RunnerUnavailable,
        ));
    };
    let account_id = planned.target.input.account_id();
    let reservation_id = Uuid::new_v4();
    let now = Utc::now();
    match repo
        .reserve_account(
            scope,
            account_id,
            reservation_id,
            now,
            now + geo_domain::CHANNEL_EXECUTION_LEASE,
        )
        .await
    {
        Ok(()) => {}
        Err(error) if error.code == ErrorCode::Conflict => {
            return Ok(ChannelDispatchResult::Deferred(
                ChannelDispatchDeferred::AccountBusy,
            ));
        }
        Err(error) => return Err(error),
    }
    let result =
        execute_reserved_channel_target(state, scope, target_id, planned, reservation_id, bridge)
            .await;
    // A claimed unknown (including a lost runner response) may still be in
    // flight remotely. Keep the reservation until expiry so another target
    // cannot start the same account while the runner's deadline elapses.
    let release = match &result {
        Ok(ChannelDispatchResult::Executed(view)) => view
            .attempts
            .last()
            .and_then(|attempt| attempt.outcome.as_ref())
            .is_some_and(|outcome| outcome.status != ChannelOutcomeStatus::Unknown),
        _ => repo
            .get_target(scope, target_id)
            .await
            .is_ok_and(|view| view.attempts.is_empty()),
    };
    if release
        && repo
            .release_account(scope, account_id, reservation_id)
            .await
            .is_err()
    {
        tracing::warn!("channel account preflight reservation release failed");
    }
    result
}

async fn execute_reserved_channel_target(
    state: &AppState,
    scope: &TenantScope,
    target_id: Uuid,
    planned: ChannelTargetView,
    reservation_id: Uuid,
    bridge: &crate::browser_bridge::BrowserBridge,
) -> Result<ChannelDispatchResult, AppError> {
    let repo = state.channel_job_repository();
    let service = state.channel_service();
    // Another trigger may have claimed while this task waited for the account
    // preflight. It must not open a second remote context for that target.
    if !repo.get_target(scope, target_id).await?.attempts.is_empty() {
        return Err(AppError::conflict(
            "target already attempted; inspect or reconcile existing outcome",
        ));
    }
    if !legacy_connector_available(state, scope, &planned.target.input).await? {
        return Ok(ChannelDispatchResult::Deferred(
            ChannelDispatchDeferred::ConnectorUnavailable,
        ));
    }
    let account = match service
        .resolve_available_account(scope, planned.target.input.account_id())
        .await
    {
        Ok(account) => account,
        Err(error) if matches!(error.code, ErrorCode::NotFound | ErrorCode::Conflict) => {
            return Ok(ChannelDispatchResult::Deferred(
                ChannelDispatchDeferred::AccountUnavailable,
            ));
        }
        Err(error) => return Err(error),
    };
    let expected = match &planned.target.input {
        ChannelTargetInput::Publish { platform, .. }
        | ChannelTargetInput::GeneratedPublish { platform, .. } => platform.as_str(),
        ChannelTargetInput::Measure { .. } => "kimi",
    };
    if account.platform != expected
        || !account.enabled
        || account.status != ChannelStatus::Ready
        || account.platform_account_id.is_none()
    {
        return Ok(ChannelDispatchResult::Deferred(
            ChannelDispatchDeferred::AccountUnavailable,
        ));
    }
    if let ChannelTargetInput::Publish {
        source_id,
        source_version_id,
        ..
    } = &planned.target.input
    {
        let source = state
            .knowledge_repository()
            .get_source(scope, *source_id)
            .await?;
        if !source.is_some_and(|source| {
            source.purpose == KnowledgePurpose::Public && source.state == SourceState::Active
        }) || state
            .knowledge_repository()
            .get_source_version(scope, *source_id, *source_version_id)
            .await?
            .is_none()
        {
            return Ok(ChannelDispatchResult::Deferred(
                ChannelDispatchDeferred::SourceUnavailable,
            ));
        }
    }
    if matches!(
        &planned.target.input,
        ChannelTargetInput::GeneratedPublish { .. }
    ) {
        match generated_publication_preflight(state, scope, &planned.target.input).await {
            Ok(true) => {
                return Ok(ChannelDispatchResult::Deferred(
                    ChannelDispatchDeferred::FixtureOnly,
                ));
            }
            Ok(false) => {}
            Err(error) if error.message.starts_with("source ") => {
                return Ok(ChannelDispatchResult::Deferred(
                    ChannelDispatchDeferred::SourceUnavailable,
                ));
            }
            Err(error) => return Err(error),
        }
        if rich_send_required(&planned.target.input) && !rich_send_bridge_available(state) {
            return Ok(ChannelDispatchResult::Deferred(
                ChannelDispatchDeferred::ConnectorUnavailable,
            ));
        }
        if !generated_connector_available(state, scope, &planned.target.input).await? {
            return Ok(ChannelDispatchResult::Deferred(
                ChannelDispatchDeferred::ConnectorUnavailable,
            ));
        }
    }
    // Opening and verifying an ephemeral browser context is reversible; no
    // publication or measurement is sent before the durable claim. This also
    // checks runner/cipher/session availability without consuming the attempt.
    let attempt_id = Uuid::new_v4();
    let opened = if planned.target.input.is_publication() {
        service
            .resume_available_browser_bound(scope, account.account_id, attempt_id)
            .await
            .map(|(session, binding, version)| (session, Some(binding), version))
    } else {
        service
            .resume_available_browser_with_renewal(scope, account.account_id)
            .await
            .map(|(session, version)| (session, None, version))
    };
    let (session, binding, mut session_version) = match opened {
        Ok(opened) => opened,
        Err(error)
            if matches!(
                error.code,
                ErrorCode::Conflict
                    | ErrorCode::CapabilityMissing
                    | ErrorCode::NotFound
                    | ErrorCode::DependencyUnavailable
            ) =>
        {
            return Ok(ChannelDispatchResult::Deferred(
                ChannelDispatchDeferred::AccountUnavailable,
            ));
        }
        Err(error) => return Err(error),
    };
    let identity = bridge.complete(session).await;
    let verified = matches!(
        identity,
        Ok(ref result) if Some(result.identity.platform_account_id.as_str()) == account.platform_account_id.as_deref()
    );
    if !verified {
        if bridge.close(session).await.is_err() {
            tracing::warn!("browser preflight session cleanup failed");
        }
        return Ok(ChannelDispatchResult::Deferred(
            ChannelDispatchDeferred::AccountUnavailable,
        ));
    }
    if let Ok(ref identity) = identity
        && let Err(error) = service
            .persist_browser_renewal(scope, &mut session_version, identity)
            .await
    {
        let _ = bridge.close(session).await;
        return Err(error);
    }
    let (target, attempt) = match repo
        .claim_reserved(scope, target_id, attempt_id, reservation_id, Utc::now())
        .await
    {
        Ok(claimed) => claimed,
        Err(error) => {
            if bridge.close(session).await.is_err() {
                tracing::warn!("browser preflight session cleanup failed");
            }
            return Err(error);
        }
    };
    let (operation, payload) = match &target.input {
        ChannelTargetInput::Publish { title, body, .. } => {
            ("publish", json!({"title":title,"body":body}))
        }
        ChannelTargetInput::GeneratedPublish {
            title,
            body,
            rich_payload: None,
            ..
        } => ("publish", json!({"title":title,"body":body})),
        // Rich content takes a distinct authenticated multipart route below.
        ChannelTargetInput::GeneratedPublish {
            rich_payload: Some(_),
            ..
        } => ("publish", json!({})),
        ChannelTargetInput::Measure {
            account_id,
            provider,
            model,
            surface,
            search_mode,
            protocol_version,
            question_set_version,
            question,
            market,
            language,
            scheduled_at,
            sample_ordinal,
            ..
        } => (
            "measure",
            json!({"target_id":target.target_id,"account_id":account_id,"provider":provider,"model":model,"surface":surface,"search_mode":search_mode,"protocol_version":protocol_version,"question_set_version":question_set_version,"question":question,"market":market,"language":language,"scheduled_at":scheduled_at,"sample_ordinal":sample_ordinal}),
        ),
    };
    let now = Utc::now();
    // Recheck mutable eligibility after the claim. Even a withdrawal at this
    // point must leave an honest attempted outcome, not release the one-shot.
    let mut execution_started = false;
    let resolved = async {
        // Persist the encrypted network/identity selected for this exact
        // preflight before sending. A crash cannot resume on a new account or
        // silently fall back to a different network.
        let binding_sha256 = binding
            .as_ref()
            .map(|binding| sha256_hex(binding.encrypted_bytes()));
        if let Some(binding) = binding {
            repo.store_publication_binding(scope, target_id, attempt.attempt_id, binding)
                .await
                .map_err(|error| {
                    AppError::new(error.code, "publication binding persistence unavailable")
                })?;
        }
        if let ChannelTargetInput::Publish {
            source_id,
            source_version_id,
            ..
        } = &target.input
        {
            let source = state
                .knowledge_repository()
                .get_source(scope, *source_id)
                .await?
                .ok_or_else(|| AppError::conflict("source withdrawn"))?;
            if source.purpose != KnowledgePurpose::Public || source.state != SourceState::Active {
                return Err(AppError::conflict("source no longer public"));
            }
            state
                .knowledge_repository()
                .get_source_version(scope, *source_id, *source_version_id)
                .await?
                .ok_or_else(|| AppError::conflict("source version no longer available"))?;
        }
        if !legacy_connector_available(state, scope, &target.input).await? {
            return Err(AppError::conflict("connector no longer available"));
        }
        if matches!(&target.input, ChannelTargetInput::GeneratedPublish { .. })
            && generated_publication_preflight(state, scope, &target.input).await?
        {
            return Err(AppError::conflict("source fixture cannot publish"));
        }
        if !generated_connector_available(state, scope, &target.input).await? {
            return Err(AppError::conflict("connector no longer available"));
        }
        if let ChannelTargetInput::GeneratedPublish {
            account_id,
            publication_intent_id,
            title,
            body,
            payload_hash,
            rich_payload: Some(rich_payload),
            ..
        } = &target.input
        {
            let callback = state
                .publication_send_callback
                .as_ref()
                .ok_or_else(|| AppError::conflict("rich publication callback unavailable"))?;
            let ticket = callback
                .register_ticket(
                    scope,
                    &AuthorizePublicationSend {
                        target_id,
                        attempt_id: attempt.attempt_id,
                        account_id: *account_id,
                        runner_session_id: session,
                        publication_intent_id: *publication_intent_id,
                        payload_hash: payload_hash.clone(),
                        encrypted_binding_sha256: binding_sha256
                            .ok_or_else(|| AppError::conflict("publication binding unavailable"))?,
                    },
                    attempt.claimed_at + chrono::Duration::minutes(5),
                )
                .await?;
            let keys = rich_payload
                .media
                .iter()
                .map(|item| item.object.clone())
                .collect::<Vec<_>>();
            let snapshots = state
                .content_media_repository()
                .snapshot_authorized_images(scope, &keys)
                .await?;
            execution_started = true;
            let result = async {
                let identity = bridge.complete(session).await?;
                if Some(identity.identity.platform_account_id.as_str())
                    != account.platform_account_id.as_deref()
                {
                    return Err(AppError::conflict("account identity changed"));
                }
                service
                    .persist_browser_renewal(scope, &mut session_version, &identity)
                    .await?;
                bridge
                    .execute_rich(
                        attempt.attempt_id,
                        session,
                        &ticket,
                        crate::browser_bridge::RichExecutionVariant {
                            title,
                            markdown: body,
                            payload_hash,
                        },
                        rich_payload,
                        snapshots,
                    )
                    .await
            }
            .await;
            retain_browser_renewal(bridge, session, service, scope, &mut session_version).await;
            if bridge.close(session).await.is_err() {
                tracing::warn!("browser execution session cleanup failed");
            }
            return result;
        }
        let source_capture_ticket = if operation == "measure" {
            state
                .observation_capture_callback()
                .map(|service| {
                    service.issue_ticket(
                        scope,
                        crate::observation_capture_callback::ObservationCaptureBinding {
                            target_id,
                            attempt_id: attempt.attempt_id,
                            account_id: account.account_id,
                            runner_session_id: session,
                            original_identity: geo_domain::ObservationProviderIdentity {
                                provider: account.platform.clone(),
                                platform_account_id: account
                                    .platform_account_id
                                    .clone()
                                    .ok_or_else(|| {
                                        AppError::conflict("account identity unavailable")
                                    })?,
                            },
                        },
                        attempt.claimed_at + geo_domain::CHANNEL_MEASUREMENT_LEASE,
                    )
                })
                .transpose()?
        } else {
            None
        };
        execution_started = true;
        execute_and_close_with_cleanup_and_ticket(
            bridge,
            session,
            account.platform_account_id.as_deref(),
            attempt.attempt_id,
            operation,
            &payload,
            ExecutionSessionContext {
                source_capture_ticket: source_capture_ticket.as_deref(),
                renewal: Some((service, scope, &mut session_version)),
            },
        )
        .await
        .0
    }
    .await;
    if !execution_started && bridge.close(session).await.is_err() {
        tracing::warn!("browser execution session cleanup failed");
    }
    let received_at = Utc::now();
    let outcome = match resolved {
        Ok(mut result) => {
            let matched = result.execution_id == attempt.attempt_id;
            let verified = matched
                && result
                    .occurred_at
                    .is_some_and(|at| at >= attempt.claimed_at && at <= received_at)
                && publication_readback(&result, &target.input);
            let observation = if matched {
                measurement_observation(&result, &target, attempt.claimed_at, received_at)
            } else {
                None
            };
            let status = match result.status.as_str() {
                "unsupported" if matched => ChannelOutcomeStatus::Unsupported,
                "login_required" | "challenge" if matched => ChannelOutcomeStatus::LoginRequired,
                // A matching explicit unknown may still be running remotely;
                // retain its reservation until the bounded deadline.
                "unknown" if matched => ChannelOutcomeStatus::Unknown,
                "completed" if observation.is_some() => observation.as_ref().unwrap().0,
                // A completed generic browser action is not proof of
                // publication nor valid independent search observation.
                "completed" if verified => ChannelOutcomeStatus::Verified,
                // A receipt for another execution supplies no sample for this
                // measurement. Publication stays unknown to avoid resending.
                _ if operation == "measure" => ChannelOutcomeStatus::Missing,
                _ => ChannelOutcomeStatus::Unknown,
            };
            let (raw_answer, citations) = observation
                .map(|(_, answer, citations)| (Some(answer), citations))
                .unwrap_or((None, vec![]));
            // Adapter evidence must never supply a Rust-owned receipt marker.
            // Retain genuine raw evidence and add one normalized marker only
            // for the attempted execution identity.
            let marker_spoofed = result
                .evidence
                .iter()
                .any(|proof| proof["kind"] == "runner_receipt");
            result
                .evidence
                .retain(|proof| proof["kind"] != "runner_receipt");
            if matched {
                let provenance = match result.provenance {
                    Some(crate::browser_bridge::BrowserReceiptProvenance::Live)
                        if !marker_spoofed
                            && result.occurred_at.is_some()
                            && result.connector_version.as_deref().is_some_and(|version| {
                                !version.trim().is_empty()
                                    && version.len() <= 100
                                    && !version.starts_with("fixture")
                            }) =>
                    {
                        "live"
                    }
                    Some(crate::browser_bridge::BrowserReceiptProvenance::Fixture) => "fixture",
                    _ => "unknown",
                };
                result.evidence.push(json!({
                    "kind":"runner_receipt",
                    "schema_version":"geo.runner.receipt.v1",
                    "provenance":provenance,
                    "execution_id":result.execution_id,
                    "connector_version":result.connector_version,
                    "occurred_at":result.occurred_at,
                }));
            }
            // A receipt without proven live origin is never allowed to look
            // like a verified non-fixture outcome in historical reports.
            let fixture = !matched
                || marker_spoofed
                || result.provenance != Some(crate::browser_bridge::BrowserReceiptProvenance::Live)
                || result.occurred_at.is_none()
                || result.connector_version.as_deref().is_none_or(|version| {
                    version.trim().is_empty()
                        || version.len() > 100
                        || version.starts_with("fixture")
                });
            // Untrusted or malformed hints must not persist raw URLs (which
            // could contain account tokens), nor acquire success semantics.
            retain_publication_candidate(
                &mut result,
                &target,
                &attempt,
                received_at,
                fixture,
                status,
            );
            ChannelOutcome {
                status,
                detail: result.reason.or_else(|| {
                    Some(if verified {
                        "public readback verified".into()
                    } else if raw_answer.is_some() {
                        "official search observation verified".into()
                    } else {
                        "runner did not provide verified external evidence".into()
                    })
                }),
                occurred_at: result
                    .occurred_at
                    .filter(|at| *at <= received_at)
                    .unwrap_or(now),
                raw_answer,
                citations,
                public_url: if verified { result.public_url } else { None },
                screenshot_ref: None,
                connector_version: result.connector_version,
                runner_evidence: result.evidence,
                // The marker distinguishes explicit fixture from unknown.
                fixture,
            }
        }
        Err(failure) => ChannelOutcome {
            status: if failure.message.starts_with("source ")
                || failure.message.starts_with("generated publication ")
                || failure.message.starts_with("connector ")
            {
                ChannelOutcomeStatus::Unsupported
            } else if matches!(failure.code, ErrorCode::Conflict) {
                ChannelOutcomeStatus::LoginRequired
            } else if matches!(failure.code, ErrorCode::CapabilityMissing) {
                ChannelOutcomeStatus::Unsupported
            } else if operation == "publish" {
                ChannelOutcomeStatus::Unknown
            } else {
                ChannelOutcomeStatus::Missing
            },
            detail: Some(format!("execution unavailable: {:?}", failure.code)),
            occurred_at: now,
            raw_answer: None,
            citations: vec![],
            public_url: None,
            screenshot_ref: None,
            connector_version: None,
            runner_evidence: vec![],
            // A failed/invalid response cannot provide live provenance.
            fixture: true,
        },
    };
    repo.finish(scope, target_id, attempt.attempt_id, outcome, Utc::now())
        .await
        .map(|view| ChannelDispatchResult::Executed(Box::new(view)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser_bridge::BrowserExecution;
    use axum::{
        Router,
        extract::State,
        http::{Method, StatusCode, Uri},
        routing::any,
    };
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[test]
    fn legacy_target_id_golden_bytes_remain_unchanged_when_binding_is_absent() {
        let wire = r#"{"kind":"measure","account_id":"00000000-0000-0000-0000-000000000001","provider":"kimi","model":"fixed","surface":"consumer_web","search_mode":"web_search","protocol_version":"v1","question_set_version":"free-text-version","question":"Question?","market":"CN","language":"en","scheduled_at":"2026-09-25T00:00:00Z","sample_ordinal":0}"#;
        let input: ChannelTargetInput = serde_json::from_str(wire).unwrap();
        assert_eq!(serde_json::to_string(&input).unwrap(), wire);
        assert_eq!(
            target_id(Uuid::nil(), &input).unwrap(),
            Uuid::parse_str("89573342-e0c3-b1e9-09df-52760630125c").unwrap()
        );
        let mut bound = input.clone();
        if let ChannelTargetInput::Measure {
            question_binding, ..
        } = &mut bound
        {
            *question_binding = Some(geo_domain::FrozenQuestionBinding {
                reference: QuestionReference {
                    question_set_id: Uuid::new_v4(),
                    question_set_version_id: Uuid::new_v4(),
                    question_id: Uuid::new_v4(),
                    question_revision_id: Uuid::new_v4(),
                },
                purpose: geo_domain::QuestionPurpose::Optimization,
                split_policy_version: "project_registry_nfkc_v1".into(),
            });
        }
        assert_ne!(
            target_id(Uuid::nil(), &input).unwrap(),
            target_id(Uuid::nil(), &bound).unwrap()
        );
    }

    #[derive(Clone)]
    struct RunnerStub {
        calls: Arc<Mutex<Vec<String>>>,
        fail: Option<&'static str>,
    }

    async fn runner_stub(
        State(state): State<RunnerStub>,
        method: Method,
        uri: Uri,
        payload: Option<Json<serde_json::Value>>,
    ) -> (StatusCode, Json<serde_json::Value>) {
        let path = uri.path();
        state.calls.lock().await.push(format!("{method} {path}"));
        if state.fail == Some("start") && method == Method::POST && path == "/v1/sessions" {
            return (StatusCode::OK, Json(json!({"invalid":"start response"})));
        }
        if state.fail == Some("complete") && path.ends_with("/complete") {
            return (StatusCode::CONFLICT, Json(json!({"error":"needs_login"})));
        }
        if state.fail == Some("post_complete")
            && path.ends_with("/complete")
            && state
                .calls
                .lock()
                .await
                .iter()
                .any(|call| call == "POST /v1/executions")
        {
            return (StatusCode::CONFLICT, Json(json!({"error":"needs_login"})));
        }
        if state.fail == Some("execute") && path == "/v1/executions" {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":"runner_unavailable"})),
            );
        }
        if method == Method::POST && path.ends_with("/complete") {
            return (
                StatusCode::OK,
                Json(json!({
                    "identity":{"platform_account_id":"verified-id","display_name":"Verified"},
                    "storage_state":{"cookies":[],"origins":[]}
                })),
            );
        }
        if method == Method::POST && path == "/v1/executions" {
            return (
                StatusCode::OK,
                Json(json!({
                    "execution_id": payload.and_then(|Json(value)|
                        value.get("execution_id").and_then(|value|value.as_str()).map(str::to_owned)
                    ),
                    "status":"unsupported","provenance":"fixture","evidence":[]
                })),
            );
        }
        if method == Method::POST && path == "/v1/sessions" {
            return (StatusCode::OK, Json(json!({"invalid":"start response"})));
        }
        (StatusCode::OK, Json(json!({"closed":true})))
    }

    async fn stub_bridge(
        fail: Option<&'static str>,
    ) -> (
        crate::browser_bridge::BrowserBridge,
        Arc<Mutex<Vec<String>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let state = RunnerStub {
            calls: calls.clone(),
            fail,
        };
        let app = Router::new().fallback(any(runner_stub)).with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (
            crate::browser_bridge::BrowserBridge::new(
                format!("http://{address}"),
                "test-token".into(),
            )
            .unwrap(),
            calls,
            server,
        )
    }

    #[tokio::test]
    async fn execution_retains_renewal_without_losing_results_when_post_send_capture_fails() {
        for fail in [None, Some("post_complete")] {
            let (bridge, calls, server) = stub_bridge(fail).await;
            let service = crate::channels::ChannelService::development();
            let scope = TenantScope::new(
                Uuid::new_v4().into(),
                Uuid::new_v4().into(),
                Some(Uuid::new_v4().into()),
            );
            let account_id = Uuid::new_v4();
            let mut version = geo_domain::ChannelSessionVersion {
                account_id,
                owner_kind: geo_domain::ChannelOwnerKind::Customer,
                platform: "kimi".into(),
                platform_account_id: "verified-id".into(),
                session: geo_domain::ChannelSecret::new(vec![1]),
            };
            let now = Utc::now();
            service
                .repository
                .save_account(
                    &scope,
                    geo_domain::ChannelAccountRecord {
                        account: geo_domain::ChannelAccount {
                            account_id,
                            project_id: scope.project_id.unwrap(),
                            owner_kind: version.owner_kind,
                            platform: version.platform.clone(),
                            group_id: None,
                            status: geo_domain::ChannelStatus::Ready,
                            display_name: None,
                            platform_account_id: Some(version.platform_account_id.clone()),
                            avatar_url: None,
                            enabled: true,
                            proxy_configured: false,
                            proxy_server: None,
                            created_at: now,
                            updated_at: now,
                        },
                        session: Some(version.session.clone()),
                        proxy: None,
                    },
                )
                .await
                .unwrap();
            let session = Uuid::new_v4();
            let (result, closed) = execute_and_close_with_cleanup_and_ticket(
                &bridge,
                session,
                Some("verified-id"),
                Uuid::new_v4(),
                "measure",
                &json!({}),
                ExecutionSessionContext {
                    source_capture_ticket: None,
                    renewal: Some((&service, &scope, &mut version)),
                },
            )
            .await;
            assert_eq!(result.unwrap().status, "unsupported");
            assert!(closed);
            let retained = service
                .repository
                .get_account(&scope, account_id)
                .await
                .unwrap()
                .session
                .unwrap();
            assert_ne!(retained.encrypted_bytes(), &[1]);
            assert_eq!(
                calls
                    .lock()
                    .await
                    .iter()
                    .filter(|call| call.ends_with("/complete"))
                    .count(),
                2
            );
            server.abort();
        }
    }

    #[tokio::test]
    async fn closes_execution_context_after_success_identity_mismatch_and_errors() {
        for fail in [None, Some("complete"), Some("execute")] {
            let (bridge, calls, server) = stub_bridge(fail).await;
            let session = Uuid::new_v4();
            let expected = if fail.is_none() {
                Some("different-id")
            } else {
                Some("verified-id")
            };
            let result = execute_and_close(
                &bridge,
                session,
                expected,
                Uuid::new_v4(),
                "publish",
                &json!({"title":"t","body":"b"}),
            )
            .await;
            assert!(result.is_err());
            let calls = calls.lock().await.clone();
            assert_eq!(
                calls.last().unwrap(),
                &format!("DELETE /v1/sessions/{session}")
            );
            if fail == Some("complete") || fail.is_none() {
                assert!(!calls.iter().any(|call| call == "POST /v1/executions"));
            }
            server.abort();
        }
        let (bridge, calls, server) = stub_bridge(None).await;
        let session = Uuid::new_v4();
        let completed = execute_and_close(
            &bridge,
            session,
            Some("verified-id"),
            Uuid::new_v4(),
            "publish",
            &json!({"title":"t","body":"b"}),
        )
        .await;
        assert_eq!(completed.unwrap().status, "unsupported");
        assert_eq!(
            calls.lock().await.last().unwrap(),
            &format!("DELETE /v1/sessions/{session}")
        );
        server.abort();
    }

    #[tokio::test]
    async fn failed_start_with_possible_remote_context_is_closed() {
        let (bridge, calls, server) = stub_bridge(Some("start")).await;
        let session = Uuid::new_v4();
        assert!(bridge.start(session, "zhihu", None, None).await.is_err());
        assert_eq!(
            calls.lock().await.last().unwrap(),
            &format!("DELETE /v1/sessions/{session}")
        );
        server.abort();
    }

    #[test]
    fn publication_candidate_is_scoped_unverified_and_strictly_sanitized() {
        let target = ChannelTarget {
            target_id: Uuid::new_v4(),
            input: ChannelTargetInput::Publish {
                source_id: Uuid::new_v4(),
                source_version_id: Uuid::new_v4(),
                platform: "zhihu".into(),
                account_id: Uuid::new_v4(),
                title: " source ".into(),
                body: "body\ntext".into(),
                body_sha256: sha256_hex(b"body\ntext"),
            },
        };
        let now = Utc::now();
        let attempt_id = Uuid::new_v4();
        let hint = json!({
            "kind":"publication_candidate",
            "schema_version":"geo.publication.candidate.v1",
            "url":"https://zhuanlan.zhihu.com/p/123",
            "expected_sha256":sha256_hex(b"source\nbody text"),
            "observed_at":now,
            "source":"post_submit_navigation",
        });
        let parse = |value: &serde_json::Value| {
            publication_candidate(value, &target, attempt_id, now, now, "connector.v1")
        };
        let saved = parse(&hint).unwrap();
        assert_eq!(saved["attempt_id"], json!(attempt_id));
        assert_eq!(saved["target_id"], json!(target.target_id));
        assert_eq!(saved["account_id"], json!(target.input.account_id()));
        assert_eq!(saved["connector_version"], "connector.v1");
        assert!(saved.get("public_url").is_none());
        assert!(saved.get("status").is_none());
        let attempt = geo_domain::ChannelAttempt {
            attempt_id,
            target_id: target.target_id,
            claimed_at: now,
            outcome: None,
            received_at: None,
        };
        for (fixture, status, hint_count, expected_count) in [
            (false, ChannelOutcomeStatus::Unknown, 1, 1),
            (true, ChannelOutcomeStatus::Unknown, 1, 0),
            (false, ChannelOutcomeStatus::Verified, 1, 0),
            (false, ChannelOutcomeStatus::Unknown, 2, 0),
        ] {
            let mut result = BrowserExecution {
                execution_id: attempt_id,
                provenance: Some(crate::browser_bridge::BrowserReceiptProvenance::Live),
                status: "unknown".into(),
                reason: None,
                evidence: vec![hint.clone(); hint_count],
                public_url: None,
                occurred_at: Some(now),
                connector_version: Some("connector.v1".into()),
                stage: Some("readback".into()),
            };
            retain_publication_candidate(&mut result, &target, &attempt, now, fixture, status);
            assert_eq!(result.evidence.len(), expected_count);
            assert!(result.public_url.is_none());
            assert_eq!(result.status, "unknown");
            assert!(!publication_readback(&result, &target.input));
        }
        for url in [
            "http://zhuanlan.zhihu.com/p/123",
            "https://untrusted.zhihu.com/p/123",
            "https://user:secret@zhuanlan.zhihu.com/p/123",
            "https://zhuanlan.zhihu.com:8443/p/123",
            "https://zhuanlan.zhihu.com/p/123?token=example",
            "https://zhuanlan.zhihu.com/p/123#fragment",
            "https://zhuanlan.zhihu.com/p/123/",
            "https://zhuanlan.zhihu.com/p/%31",
            "https://zhuanlan.zhihu.com/p/",
        ] {
            let mut invalid = hint.clone();
            invalid["url"] = json!(url);
            assert!(parse(&invalid).is_none(), "{url}");
        }
        for (field, value) in [
            ("source", json!("existing_article")),
            ("expected_sha256", json!(sha256_hex(b"other body"))),
            ("observed_at", json!(now - chrono::Duration::seconds(1))),
            ("observed_at", json!(now + chrono::Duration::seconds(1))),
            ("account_id", json!(Uuid::new_v4())),
            ("schema_version", json!("other.v1")),
        ] {
            let mut invalid = hint.clone();
            invalid[field] = value;
            assert!(parse(&invalid).is_none(), "{field}");
        }
    }

    #[test]
    fn completed_without_owned_public_readback_is_not_verified() {
        let input = ChannelTargetInput::Publish {
            source_id: Uuid::new_v4(),
            source_version_id: Uuid::new_v4(),
            platform: "zhihu".into(),
            account_id: Uuid::new_v4(),
            title: "source".into(),
            body: "body".into(),
            body_sha256: sha256_hex(b"body"),
        };
        let receipt = BrowserExecution {
            execution_id: Uuid::new_v4(),
            provenance: Some(crate::browser_bridge::BrowserReceiptProvenance::Live),
            status: "completed".into(),
            reason: None,
            evidence: vec![json!({
                "kind":"public_readback","url":"https://www.zhihu.com/p/123",
                "content_matched":true,"owned_by_account":false,
                "expected_sha256":sha256_hex(b"source\nbody"),"readback_sha256":sha256_hex(b"source\nbody"),
            })],
            public_url: Some("https://www.zhihu.com/p/123".into()),
            occurred_at: Some(Utc::now()),
            connector_version: Some("unverified.v1".into()),
            stage: Some("public_readback".into()),
        };
        assert!(!publication_readback(&receipt, &input));
        let receipt = BrowserExecution {
            evidence: vec![json!({
                "kind":"public_readback","url":"https://www.zhihu.com/p/123",
                "content_matched":true,"owned_by_account":true,
                "expected_sha256":sha256_hex(b"source\nbody"),"readback_sha256":sha256_hex(b"source\nbody"),
            })],
            ..receipt
        };
        assert!(publication_readback(&receipt, &input));
    }

    #[test]
    fn rich_send_needs_separate_runner_operation_and_callback_configuration() {
        let state = AppState::development();
        assert!(!rich_send_bridge_available(&state));
        assert!(state.observation_capture_callback().is_none());
        let key = ConnectorKey {
            platform_id: "zhihu".into(),
            placement_slot: "primary".into(),
        };
        let connector = crate::browser_bridge::RunnerConnector {
            platform: "zhihu".into(),
            placement_slot: "primary".into(),
            connector_version: "adapter.v1".into(),
            operations: vec!["publish".into()],
            verified: true,
        };
        assert!(!rich_runner_operation_available(
            std::slice::from_ref(&connector),
            &key,
            "adapter.v1"
        ));
        let mut rich = connector;
        rich.operations.push("rich_publish".into());
        assert!(rich_runner_operation_available(
            std::slice::from_ref(&rich),
            &key,
            "adapter.v1"
        ));
        assert!(!rich_runner_operation_available(
            &[rich],
            &key,
            "adapter.v2"
        ));
    }

    #[test]
    fn generated_readback_requires_exact_frozen_title_and_body() {
        let input = ChannelTargetInput::GeneratedPublish {
            content_revision_id: Uuid::new_v4(),
            variant_id: Uuid::new_v4(),
            publication_intent_id: Uuid::new_v4(),
            distribution_target_id: Uuid::new_v4(),
            origin_request_id: None,
            platform: "zhihu".into(),
            account_id: Uuid::new_v4(),
            title: "Frozen title".into(),
            body: "Frozen body".into(),
            body_sha256: sha256_hex(b"Frozen body"),
            payload_hash: "frozen".into(),
            evidence: vec![],
            rich_payload: None,
        };
        let exact_hash = sha256_hex(b"Frozen title\nFrozen body");
        let proof = |hash: &str| {
            json!({
                "kind":"public_readback",
                "url":"https://zhuanlan.zhihu.com/p/321",
                "content_matched":true,
                "owned_by_account":true,
                "expected_sha256":hash,
                "readback_sha256":hash
            })
        };
        let receipt = BrowserExecution {
            execution_id: Uuid::new_v4(),
            provenance: Some(crate::browser_bridge::BrowserReceiptProvenance::Live),
            status: "completed".into(),
            reason: None,
            evidence: vec![proof(&exact_hash)],
            public_url: Some("https://zhuanlan.zhihu.com/p/321".into()),
            occurred_at: Some(Utc::now()),
            connector_version: Some("test-runner".into()),
            stage: Some("public_readback".into()),
        };
        assert!(publication_readback(&receipt, &input));
        let mut rich = input.clone();
        if let ChannelTargetInput::GeneratedPublish {
            content_revision_id,
            rich_payload,
            ..
        } = &mut rich
        {
            *rich_payload = Some(geo_domain::RichPublicationPayload {
                schema_version: 2,
                format: RICH_MARKDOWN_FORMAT.into(),
                content_revision_id: *content_revision_id,
                policy_version: geo_domain::RICH_CHANNEL_VARIANT_POLICY.into(),
                document: geo_domain::StructuredDocument {
                    title: "Frozen title".into(),
                    blocks: vec![],
                    schema_version: Some(2),
                },
                media: vec![],
            });
        }
        assert!(rich_send_required(&rich));
        assert!(!rich_send_bridge_available(&AppState::development()));
        assert!(!publication_readback(&receipt, &rich));
        let probe = ChannelTarget {
            target_id: Uuid::new_v4(),
            input: rich,
        };
        assert!(
            publication_candidate(
                &json!({"kind":"publication_candidate"}),
                &probe,
                Uuid::new_v4(),
                Utc::now(),
                Utc::now(),
                "test-runner",
            )
            .is_none()
        );
        let fixture = BrowserExecution {
            provenance: Some(crate::browser_bridge::BrowserReceiptProvenance::Fixture),
            ..receipt
        };
        assert!(!publication_readback(&fixture, &input));
        let missing = BrowserExecution {
            provenance: None,
            ..fixture
        };
        assert!(!publication_readback(&missing, &input));
        let spoof = BrowserExecution {
            provenance: Some(crate::browser_bridge::BrowserReceiptProvenance::Live),
            evidence: vec![
                proof(&exact_hash),
                json!({"kind":"runner_receipt","provenance":"live"}),
            ],
            ..missing
        };
        assert!(!publication_readback(&spoof, &input));
        let wrong = BrowserExecution {
            evidence: vec![proof(&sha256_hex(b"Later draft\nFrozen body"))],
            ..spoof
        };
        assert!(!publication_readback(&wrong, &input));
        let wrong = BrowserExecution {
            evidence: vec![proof(&exact_hash)],
            public_url: Some("https://untrusted.zhihu.com/p/321".into()),
            ..wrong
        };
        assert!(!publication_readback(&wrong, &input));
    }
}
