//! Channel planning/dispatch. Only source versions and frozen questions are
//! accepted publicly; outcomes are exclusively written from the runner.

use axum::{
    Json,
    extract::{Extension, Path, State},
};
use chrono::Utc;
use geo_domain::{
    AppError, ChannelOutcome, ChannelOutcomeStatus, ChannelPlan, ChannelStatus, ChannelTarget,
    ChannelTargetInput, ChannelTargetView, ErrorCode, KnowledgePurpose, ProjectId, ProjectStatus,
    SourceState, TenantScope, sha256_hex,
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{ApiError, AppState, AuthContext, RequestContext, api_error, require_project_writer};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanRequest {
    pub publications: Vec<PublicationRequest>,
    pub measurements: Vec<MeasurementRequest>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationRequest {
    pub source_id: Uuid,
    pub source_version_id: Uuid,
    pub platform: String,
    pub account_id: Uuid,
}

#[derive(Debug, Deserialize)]
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
}

#[derive(Debug)]
pub enum ChannelDispatchResult {
    Executed(Box<ChannelTargetView>),
    Deferred(ChannelDispatchDeferred),
}

fn error(error: AppError, context: RequestContext) -> ApiError {
    api_error(error, context.request_id)
}

async fn scope(
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

fn publication_readback(
    result: &crate::browser_bridge::BrowserExecution,
    target: &ChannelTargetInput,
) -> bool {
    let ChannelTargetInput::Publish {
        platform,
        title,
        body,
        ..
    } = target
    else {
        return false;
    };
    let normalize = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let expected_hash = sha256_hex(format!("{}\n{}", normalize(title), normalize(body)).as_bytes());
    if result.status != "completed" || result.stage.as_deref() != Some("public_readback") {
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
        || url.host_str() != Some("www.zhihu.com")
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

/// Always release the fresh browser context after a resumed account has
/// completed identity verification and its one typed operation. The runner's
/// idle reaper remains the fallback if this process dies mid-request.
async fn execute_and_close(
    bridge: &crate::browser_bridge::BrowserBridge,
    session: Uuid,
    expected_identity: Option<&str>,
    attempt_id: Uuid,
    operation: &str,
    payload: &serde_json::Value,
) -> Result<crate::browser_bridge::BrowserExecution, AppError> {
    let result = async {
        let verified = bridge.complete(session).await?;
        if Some(verified.identity.platform_account_id.as_str()) != expected_identity {
            return Err(AppError::conflict("account identity changed"));
        }
        bridge
            .execute(attempt_id, session, operation, payload)
            .await
    }
    .await;
    if bridge.close(session).await.is_err() {
        // A successful external result is still evidence if cleanup fails.
        // The runner owns a bounded idle reaper as the crash/outage fallback.
        tracing::warn!("browser execution session cleanup failed");
    }
    result
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

async fn measurement_input(
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
    })
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
    if request.publications.len() + request.measurements.len() > 100 {
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
            now + chrono::Duration::minutes(5),
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
        ChannelTargetInput::Publish { platform, .. } => platform.as_str(),
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
    // Opening and verifying an ephemeral browser context is reversible; no
    // publication or measurement is sent before the durable claim. This also
    // checks runner/cipher/session availability without consuming the attempt.
    let session = match service
        .resume_available_browser(scope, account.account_id)
        .await
    {
        Ok(session) => session,
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
    let (target, attempt) = match repo
        .claim_reserved(scope, target_id, Uuid::new_v4(), reservation_id, Utc::now())
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
        ChannelTargetInput::Measure {
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
            json!({"provider":provider,"model":model,"surface":surface,"search_mode":search_mode,"protocol_version":protocol_version,"question_set_version":question_set_version,"question":question,"market":market,"language":language,"scheduled_at":scheduled_at,"sample_ordinal":sample_ordinal}),
        ),
    };
    let now = Utc::now();
    // Recheck mutable eligibility after the claim. Even a withdrawal at this
    // point must leave an honest attempted outcome, not release the one-shot.
    let resolved = async {
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
        execute_and_close(
            bridge,
            session,
            account.platform_account_id.as_deref(),
            attempt.attempt_id,
            operation,
            &payload,
        )
        .await
    }
    .await;
    if resolved
        .as_ref()
        .is_err_and(|error| error.message.starts_with("source "))
        && bridge.close(session).await.is_err()
    {
        tracing::warn!("browser execution session cleanup failed");
    }
    let outcome = match resolved {
        Ok(result) => {
            let matched = result.execution_id == attempt.attempt_id;
            let verified = matched && publication_readback(&result, &target.input);
            let status = match result.status.as_str() {
                "unsupported" if matched => ChannelOutcomeStatus::Unsupported,
                "login_required" | "challenge" if matched => ChannelOutcomeStatus::LoginRequired,
                "unknown" => ChannelOutcomeStatus::Unknown,
                // A completed generic browser action is not proof of
                // publication nor valid independent search observation.
                "completed" if verified => ChannelOutcomeStatus::Verified,
                _ => ChannelOutcomeStatus::Unknown,
            };
            ChannelOutcome {
                status,
                detail: result.reason.or_else(|| {
                    Some(if verified {
                        "public readback verified".into()
                    } else {
                        "runner did not provide verified external evidence".into()
                    })
                }),
                occurred_at: result.occurred_at.unwrap_or(now),
                raw_answer: None,
                citations: vec![],
                public_url: if verified { result.public_url } else { None },
                screenshot_ref: None,
                connector_version: result.connector_version,
                runner_evidence: result.evidence,
                fixture: false,
            }
        }
        Err(failure) => ChannelOutcome {
            status: if failure.message.starts_with("source ") {
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
            fixture: false,
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
                    "status":"unsupported","evidence":[]
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
}
