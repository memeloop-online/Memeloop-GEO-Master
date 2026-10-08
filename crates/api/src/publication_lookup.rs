//! A claimed lookup may observe a public asset, but cannot certify the
//! original send or mutate its immutable publication receipt.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Extension, Path, Query, State},
};
use chrono::{DateTime, Duration, Utc};
use geo_domain::{
    AppError, ChannelTargetInput, ErrorCode, ProjectId, PublicationLookupFinding,
    PublicationLookupJob, PublicationLookupObservation, PublicationLookupRepository, TenantScope,
    sha256_hex,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    ApiError, AppState, RequestContext, api_error,
    browser_bridge::{BrowserExecution, BrowserReceiptProvenance},
    channel_jobs::{execute_and_close_with_cleanup, publication_readback},
};

const LEASE: Duration = Duration::minutes(5);
const PAGE_SIZE: usize = 20;

#[derive(Deserialize)]
pub struct LookupPageQuery {
    before: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct LookupRead {
    pub(crate) target_id: Uuid,
    pub(crate) attempt_id: Option<Uuid>,
    pub(crate) job: Option<LookupJobRead>,
    pub(crate) observations: Vec<LookupObservationRead>,
    pub(crate) next_before: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub(crate) struct LookupJobRead {
    pub(crate) query_count: i32,
    pub(crate) next_due_at: Option<DateTime<Utc>>,
    pub(crate) last_error_code: Option<&'static str>,
    pub(crate) in_progress: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct LookupObservationRead {
    pub(crate) execution_id: Uuid,
    pub(crate) finding: PublicationLookupFinding,
    pub(crate) observed_at: DateTime<Utc>,
    pub(crate) received_at: DateTime<Utc>,
    pub(crate) error_code: Option<&'static str>,
    pub(crate) public_url: Option<String>,
}

/// Only locally defined, non-secret failure classes can cross the HTTP
/// boundary; arbitrary stored codes (including future connector messages)
/// collapse to a generic class.
fn public_error_code(code: Option<&str>) -> Option<&'static str> {
    code.map(|value| match value {
        "candidate_missing" => "candidate_missing",
        "candidate_invalid" => "candidate_invalid",
        "target_mismatch" => "target_mismatch",
        "connector_version_missing" => "connector_version_missing",
        "connector_version_invalid" => "connector_version_invalid",
        "binding_missing" => "binding_missing",
        "binding_unavailable" => "binding_unavailable",
        "runner_unavailable" => "runner_unavailable",
        "account_busy" => "account_busy",
        "account_reservation_failed" => "account_reservation_failed",
        "account_or_network_unavailable" => "account_or_network_unavailable",
        "connector_version_mismatch" => "connector_version_mismatch",
        "lookup_preflight_expired" => "lookup_preflight_expired",
        "readback_unverified" => "readback_unverified",
        "lookup_unavailable" => "lookup_unavailable",
        _ => "lookup_error",
    })
}

pub(crate) async fn public_observed_url(
    state: &AppState,
    scope: &TenantScope,
    observation: &PublicationLookupObservation,
    job: &PublicationLookupJob,
) -> Option<String> {
    if matches!(
        &job.frozen_input,
        ChannelTargetInput::GeneratedPublish {
            rich_payload: Some(_),
            ..
        }
    ) {
        return None;
    }
    if observation.finding != PublicationLookupFinding::AssetObserved
        || observation.attempt_id != job.attempt_id
        || observation.observed_at > observation.received_at
        || !job.frozen_input.is_publication()
        || job.frozen_input.account_id() != job.account_id
    {
        return None;
    }
    let (platform, title, body) = match &job.frozen_input {
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
    let evidence = observation.evidence.as_object()?;
    let url = evidence.get("public_url")?.as_str()?;
    let digest = evidence.get("content_sha256")?.as_str()?;
    let normalized = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let expected_digest =
        sha256_hex(format!("{}\n{}", normalized(title), normalized(body)).as_bytes());
    let schema = evidence.get("schema_version")?.as_str()?;
    let version = evidence.get("connector_version")?.as_str()?;
    let common = evidence.get("provenance")?.as_str()? == "live"
        && evidence.get("original_attempt_id")?.as_str()? == job.attempt_id.to_string()
        && evidence.get("target_id")?.as_str()? == job.target_id.to_string()
        && evidence.get("account_id")?.as_str()? == job.account_id.to_string()
        && evidence
            .get("observed_at")?
            .as_str()?
            .parse::<DateTime<Utc>>()
            .ok()?
            .timestamp_micros()
            == observation.observed_at.timestamp_micros()
        && digest == expected_digest
        && valid_candidate(url, platform);
    if !common {
        return None;
    }
    match schema {
        "geo.publication.asset_observation.v1"
            if job.candidate_public_url.as_deref() == Some(url)
                && job.connector_version.as_deref() == Some(version) =>
        {
            Some(url.to_owned())
        }
        "geo.publication.asset_observation.v2"
            if evidence.len() == 11
                && evidence.get("discovery_kind")?.as_str()? == "own_account_list_discovery"
                && evidence.get("version_authority")?.as_str()? == "presend_encrypted_binding"
                && job
                    .connector_version
                    .as_deref()
                    .is_none_or(|saved| saved == version) =>
        {
            let binding = state
                .channel_job_repository()
                .get_publication_binding(scope, job.target_id, job.attempt_id)
                .await
                .ok()??;
            (state
                .channel_service()
                .original_publication_connector_version(
                    scope,
                    job.account_id,
                    job.attempt_id,
                    &binding,
                    platform,
                )
                .as_deref()
                == Some(version))
            .then(|| url.to_owned())
        }
        _ => None,
    }
}

/// Shared Rust projection for HTTP and later P00 reads. It never returns
/// the frozen input, evidence JSON, account, binding or lease identifiers.
pub(crate) async fn read_publication_lookup(
    state: &AppState,
    scope: &TenantScope,
    target_id: Uuid,
    before: Option<Uuid>,
) -> Result<LookupRead, AppError> {
    let target = state
        .channel_job_repository()
        .get_target(scope, target_id)
        .await?;
    if !target.target.input.is_publication() {
        return Err(AppError::invalid_request(
            "publication lookup requires a publication target",
        ));
    }
    let attempt_id = target.attempts.first().map(|attempt| attempt.attempt_id);
    let empty = || LookupRead {
        target_id,
        attempt_id,
        job: None,
        observations: vec![],
        next_before: None,
    };
    let Some(attempt_id) = attempt_id else {
        if before.is_some() {
            return Err(AppError::invalid_request("invalid lookup cursor"));
        }
        return Ok(empty());
    };
    let Some(repository) = &state.publication_lookup_repository else {
        if before.is_some() {
            return Err(AppError::invalid_request("invalid lookup cursor"));
        }
        return Ok(empty());
    };
    let job = match repository.get(scope, attempt_id).await {
        Ok(job) if job.target_id == target_id && job.attempt_id == attempt_id => job,
        Ok(_) => return Err(AppError::not_found("lookup job not found")),
        Err(error) if error.code == ErrorCode::NotFound => {
            if before.is_some() {
                return Err(AppError::invalid_request("invalid lookup cursor"));
            }
            return Ok(empty());
        }
        Err(error) => return Err(error),
    };
    let mut page = repository
        .observation_page(scope, attempt_id, before, PAGE_SIZE)
        .await?;
    let has_more = page.len() > PAGE_SIZE;
    page.truncate(PAGE_SIZE);
    let next_before = has_more.then(|| page.last().expect("page has 20 observations").execution_id);
    Ok(LookupRead {
        target_id,
        attempt_id: Some(attempt_id),
        job: Some(LookupJobRead {
            query_count: job.query_count,
            next_due_at: job.next_due_at,
            last_error_code: public_error_code(job.last_error_code.as_deref()),
            in_progress: job
                .lease_expires_at
                .is_some_and(|expires| expires > Utc::now()),
        }),
        observations: {
            let mut reads = Vec::with_capacity(page.len());
            for observation in &page {
                reads.push(LookupObservationRead {
                    execution_id: observation.execution_id,
                    finding: observation.finding,
                    observed_at: observation.observed_at,
                    received_at: observation.received_at,
                    error_code: public_error_code(observation.error_code.as_deref()),
                    public_url: public_observed_url(state, scope, observation, &job).await,
                });
            }
            reads
        },
        next_before,
    })
}

pub async fn get_publication_lookup(
    State(state): State<AppState>,
    Path((project_id, target_id)): Path<(ProjectId, Uuid)>,
    Query(query): Query<LookupPageQuery>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<LookupRead>, ApiError> {
    let scope = crate::channel_jobs::scope(&state, &tenant, project_id)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    read_publication_lookup(&state, &scope, target_id, query.before)
        .await
        .map(Json)
        .map_err(|error| api_error(error, context.request_id))
}

/// Atomically claim before creating a task: competing scans cannot accumulate
/// unclaimed browser work. `false` means another worker holds this lookup.
pub async fn dispatch_publication_lookup(
    state: AppState,
    repository: Arc<dyn PublicationLookupRepository>,
    scope: TenantScope,
    attempt_id: Uuid,
) -> Result<bool, AppError> {
    let claimed_at = Utc::now();
    let execution_id = Uuid::new_v4();
    let job = match repository
        .claim(
            &scope,
            attempt_id,
            execution_id,
            claimed_at,
            claimed_at + LEASE,
        )
        .await
    {
        Ok(job) => job,
        Err(error) if error.code == ErrorCode::Conflict => return Ok(false),
        Err(error) => return Err(error),
    };
    tokio::spawn(async move {
        if let Err(error) =
            execute_claimed_lookup(&state, repository, &scope, job, execution_id, claimed_at).await
        {
            // Do not print runner responses, URLs, account identifiers, or
            // untrusted error messages. The expired lease remains retryable.
            tracing::warn!(code = ?error.code, "publication lookup persistence failed");
        }
    });
    Ok(true)
}

async fn execute_claimed_lookup(
    state: &AppState,
    repository: Arc<dyn PublicationLookupRepository>,
    scope: &TenantScope,
    job: PublicationLookupJob,
    execution_id: Uuid,
    claimed_at: DateTime<Utc>,
) -> Result<(), AppError> {
    let (finding, evidence, code) = lookup_once(state, scope, &job, execution_id, claimed_at).await;
    let received_at = Utc::now();
    let observed_at = evidence
        .get("observed_at")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<DateTime<Utc>>().ok())
        .filter(|at| *at >= claimed_at && *at <= received_at)
        .unwrap_or(received_at);
    let delay = if finding == PublicationLookupFinding::AssetObserved {
        // An observed asset is not a verified send; keep a slow read-only
        // schedule until a separate causal reconciliation exists.
        Duration::hours(6)
    } else if code == Some("candidate_missing") {
        Duration::minutes(15)
    } else {
        let power = job.query_count.clamp(0, 6) as u32;
        Duration::minutes(2_i64.pow(power).min(60))
    };
    let observation = PublicationLookupObservation {
        execution_id,
        attempt_id: job.attempt_id,
        finding,
        evidence,
        observed_at,
        received_at,
        error_code: code.map(str::to_owned),
    };
    repository
        .finish(
            scope,
            job.attempt_id,
            observation,
            Some(received_at + delay),
        )
        .await?;
    Ok(())
}

/// All decisions after claim are read-only with respect to the original
/// attempt, project state, sources, reporting, and connector capabilities.
async fn lookup_once(
    state: &AppState,
    scope: &TenantScope,
    job: &PublicationLookupJob,
    execution_id: Uuid,
    claimed_at: DateTime<Utc>,
) -> (PublicationLookupFinding, Value, Option<&'static str>) {
    let unknown = |code| (PublicationLookupFinding::Unknown, json!({}), Some(code));
    // Existing lookup only compares normalized title/body. It must not
    // establish a rich public-readback or media capability.
    if matches!(
        &job.frozen_input,
        ChannelTargetInput::GeneratedPublish {
            rich_payload: Some(_),
            ..
        }
    ) {
        return unknown("lookup_unavailable");
    }
    let (platform, title, body) = match &job.frozen_input {
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
        } if job.frozen_input.account_id() == job.account_id => (platform, title, body),
        _ => return unknown("target_mismatch"),
    };
    // The candidate is never obtained from a caller: it is either the
    // validated immutable hint or discovered inside the bound account.
    let candidate = job.candidate_public_url.as_deref();
    if candidate.is_some_and(|url| !valid_candidate(url, platform)) {
        return unknown("candidate_invalid");
    }
    if job.connector_version.as_deref().is_some_and(|version| {
        version.is_empty() || version.len() > 100 || version.starts_with("fixture")
    }) {
        return unknown("connector_version_invalid");
    }
    let binding = match state
        .channel_job_repository()
        .get_publication_binding(scope, job.target_id, job.attempt_id)
        .await
    {
        Ok(Some(binding)) => binding,
        Ok(None) => return unknown("binding_missing"),
        Err(_) => return unknown("binding_unavailable"),
    };
    let Some(original_version) = state
        .channel_service()
        .original_publication_connector_version(
            scope,
            job.account_id,
            job.attempt_id,
            &binding,
            platform,
        )
    else {
        return unknown("binding_unavailable");
    };
    if job
        .connector_version
        .as_deref()
        .is_some_and(|version| version != original_version.as_str())
    {
        return unknown("connector_version_mismatch");
    }
    let Some(bridge) = state.channel_service().browser.as_ref() else {
        return unknown("runner_unavailable");
    };
    let reservation_id = Uuid::new_v4();
    let now = Utc::now();
    match state
        .channel_job_repository()
        .reserve_account(scope, job.account_id, reservation_id, now, now + LEASE)
        .await
    {
        Ok(()) => {}
        Err(error) if error.code == ErrorCode::Conflict => return unknown("account_busy"),
        Err(_) => return unknown("account_reservation_failed"),
    }
    // A failed start may have created a context, and is not proof of cleanup.
    // Hold the reservation until its bounded expiry on that path.
    let (session, original_identity, bound_version) = match state
        .channel_service()
        .resume_publication_lookup_browser(scope, job.account_id, job.attempt_id, &binding)
        .await
    {
        Ok(value) => value,
        Err(error) => {
            // These validation errors occur before browser startup. They must
            // not unnecessarily occupy the account's execution reservation.
            if matches!(
                error.message.as_str(),
                "publication browser binding has changed"
                    | "publication browser binding is invalid"
                    | "channel account is not ready"
                    | "channel account needs login"
                    | "channel account not assigned to project"
                    | "publication connector is unavailable"
            ) {
                release_reservation(state, scope, job.account_id, reservation_id).await;
            }
            // A failed start can itself have created a context; its client
            // tries to close by UUID, but we cannot prove cleanup succeeded.
            return unknown("account_or_network_unavailable");
        }
    };
    if bound_version != original_version {
        if bridge.close(session).await.is_ok() {
            release_reservation(state, scope, job.account_id, reservation_id).await;
        }
        return unknown("connector_version_mismatch");
    }
    // The runner may remain active for two minutes after an HTTP timeout.
    // Leave enough of both durable leases for identity checking and that
    // remote deadline before starting a read-only operation.
    if Utc::now() + Duration::minutes(3) >= claimed_at + LEASE {
        if bridge.close(session).await.is_ok() {
            release_reservation(state, scope, job.account_id, reservation_id).await;
        }
        return unknown("lookup_preflight_expired");
    }
    let payload = match candidate {
        Some(candidate) => json!({"title":title,"body":body,"public_url":candidate}),
        None => json!({"title":title,"body":body}),
    };
    let (receipt, closed) = execute_and_close_with_cleanup(
        bridge,
        session,
        Some(&original_identity),
        execution_id,
        "lookup",
        &payload,
    )
    .await;
    if closed {
        release_reservation(state, scope, job.account_id, reservation_id).await;
    }
    match receipt {
        Ok(receipt) => {
            let received_at = Utc::now();
            match observed_asset(
                &receipt,
                job,
                &bound_version,
                execution_id,
                claimed_at,
                received_at,
            ) {
                Some(evidence) => (PublicationLookupFinding::AssetObserved, evidence, None),
                None => unknown("readback_unverified"),
            }
        }
        Err(_) => unknown("lookup_unavailable"),
    }
}

async fn release_reservation(
    state: &AppState,
    scope: &TenantScope,
    account_id: Uuid,
    reservation_id: Uuid,
) {
    if state
        .channel_job_repository()
        .release_account(scope, account_id, reservation_id)
        .await
        .is_err()
    {
        tracing::warn!("publication lookup reservation cleanup failed");
    }
}

fn valid_candidate(candidate: &str, platform: &str) -> bool {
    platform == "zhihu"
        && candidate.len() <= 256
        && ["https://www.zhihu.com/p/", "https://zhuanlan.zhihu.com/p/"]
            .iter()
            .any(|prefix| {
                candidate.strip_prefix(prefix).is_some_and(|id| {
                    !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())
                })
            })
}

/// Keep only bounded, locally reconstructed fields. No arbitrary adapter
/// evidence, browser storage state, content body or credentials are persisted.
fn observed_asset(
    receipt: &BrowserExecution,
    job: &PublicationLookupJob,
    bound_version: &str,
    execution_id: Uuid,
    claimed_at: DateTime<Utc>,
    received_at: DateTime<Utc>,
) -> Option<Value> {
    if matches!(
        &job.frozen_input,
        ChannelTargetInput::GeneratedPublish {
            rich_payload: Some(_),
            ..
        }
    ) {
        return None;
    }
    let at = receipt.occurred_at?;
    let version = bound_version;
    let (platform, title, body) = match &job.frozen_input {
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
    let url = receipt.public_url.as_deref()?;
    let candidate = job.candidate_public_url.as_deref();
    if job.frozen_input.account_id() != job.account_id
        || !valid_candidate(url, platform)
        || candidate.is_some_and(|candidate| candidate != url)
        || job
            .connector_version
            .as_deref()
            .is_some_and(|saved| saved != version)
    {
        return None;
    }
    if receipt.execution_id != execution_id
        || receipt.provenance != Some(BrowserReceiptProvenance::Live)
        || receipt.connector_version.as_deref() != Some(version)
        || at < claimed_at
        || at > received_at
        || receipt
            .evidence
            .iter()
            .filter(|proof| proof["kind"] == "public_readback")
            .count()
            != 1
        || !publication_readback(receipt, &job.frozen_input)
    {
        return None;
    }
    let normalize = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let digest = sha256_hex(format!("{}\n{}", normalize(title), normalize(body)).as_bytes());
    if candidate.is_none() {
        let discovery: Vec<_> = receipt
            .evidence
            .iter()
            .filter(|proof| {
                proof.get("kind").and_then(Value::as_str) == Some("own_account_list_discovery")
            })
            .collect();
        if discovery.len() != 1 {
            return None;
        }
        let proof = discovery[0].as_object()?;
        if proof.len() != 7
            || proof.get("schema_version")?.as_str()? != "geo.publication.discovery.v1"
            || proof.get("public_url")?.as_str()? != url
            || proof.get("expected_sha256")?.as_str()? != digest
            || !proof.get("list_complete")?.as_bool()?
            || proof.get("exact_match_count")?.as_u64()? != 1
            || proof
                .get("observed_at")?
                .as_str()?
                .parse::<DateTime<Utc>>()
                .ok()?
                .timestamp_micros()
                != at.timestamp_micros()
        {
            return None;
        }
    }
    let mut evidence = json!({
        "schema_version": if candidate.is_some() { "geo.publication.asset_observation.v1" } else { "geo.publication.asset_observation.v2" },
        "public_url": url,
        "content_sha256": digest,
        "connector_version": version,
        "observed_at": at,
        "original_attempt_id": job.attempt_id,
        "target_id": job.target_id,
        "account_id": job.account_id,
        "provenance": "live",
        // Critically, no claim that the original send created this asset.
    });
    if candidate.is_none() {
        // A v2 observation is projected only after independently rechecking
        // its original encrypted binding, including no-receipt jobs.
        evidence["discovery_kind"] = json!("own_account_list_discovery");
        evidence["version_authority"] = json!("presend_encrypted_binding");
    }
    Some(evidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use axum::{
        Json, Router,
        extract::State,
        http::{Method, StatusCode, Uri},
        routing::any,
    };
    use geo_domain::{OperatorId, ProjectId, TenantId};
    use tokio::sync::Mutex;

    #[derive(Clone)]
    struct ReadOnlyLookup {
        job: PublicationLookupJob,
        history: Vec<PublicationLookupObservation>,
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
            panic!("a read must never enqueue")
        }
        async fn scan_due(
            &self,
            _: Option<Uuid>,
            _: DateTime<Utc>,
            _: usize,
        ) -> Result<Vec<geo_domain::PublicationLookupCandidate>, AppError> {
            panic!("a read must never scan")
        }
        async fn claim(
            &self,
            _: &TenantScope,
            _: Uuid,
            _: Uuid,
            _: DateTime<Utc>,
            _: DateTime<Utc>,
        ) -> Result<PublicationLookupJob, AppError> {
            panic!("a read must never claim")
        }
        async fn finish(
            &self,
            _: &TenantScope,
            _: Uuid,
            _: PublicationLookupObservation,
            _: Option<DateTime<Utc>>,
        ) -> Result<PublicationLookupJob, AppError> {
            panic!("a read must never finish")
        }
        async fn get(
            &self,
            _: &TenantScope,
            attempt: Uuid,
        ) -> Result<PublicationLookupJob, AppError> {
            (attempt == self.job.attempt_id)
                .then(|| self.job.clone())
                .ok_or_else(|| AppError::not_found("lookup job not found"))
        }
        async fn observations(
            &self,
            _: &TenantScope,
            _: Uuid,
        ) -> Result<Vec<PublicationLookupObservation>, AppError> {
            panic!("unbounded observation read is prohibited")
        }
        async fn observation_page(
            &self,
            _: &TenantScope,
            attempt: Uuid,
            before: Option<Uuid>,
            limit: usize,
        ) -> Result<Vec<PublicationLookupObservation>, AppError> {
            assert_eq!(attempt, self.job.attempt_id);
            assert_eq!(limit, PAGE_SIZE);
            let offset = before
                .map(|cursor| {
                    self.history
                        .iter()
                        .position(|row| row.execution_id == cursor)
                        .map(|index| index + 1)
                        .ok_or_else(|| AppError::invalid_request("invalid lookup cursor"))
                })
                .transpose()?
                .unwrap_or(0);
            Ok(self
                .history
                .iter()
                .skip(offset)
                .take(limit + 1)
                .cloned()
                .collect())
        }
        async fn report_asset_observations(
            &self,
            _: &TenantScope,
            _: &[Uuid],
            _: DateTime<Utc>,
        ) -> Result<Vec<geo_domain::PublicationLookupReportObservation>, AppError> {
            Ok(vec![])
        }
    }

    #[tokio::test]
    async fn read_projection_is_bounded_no_mutations_and_strips_arbitrary_evidence() {
        use geo_domain::{
            ChannelJobRepository, ChannelPlan, ChannelTarget, MemoryChannelJobRepository,
        };
        let job = job();
        let scope = TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        );
        let channels = Arc::new(MemoryChannelJobRepository::default());
        channels
            .create_plan(
                &scope,
                ChannelPlan {
                    plan_id: Uuid::new_v4(),
                    project_id: scope.project_id.unwrap(),
                    cycle_id: Uuid::new_v4(),
                    input_hash: "fixture".into(),
                    revision: 1,
                    created_at: Utc::now(),
                    targets: vec![ChannelTarget {
                        target_id: job.target_id,
                        input: job.frozen_input.clone(),
                    }],
                },
            )
            .await
            .unwrap();
        let state = AppState::development().with_channel_job_repository(channels.clone());
        let empty = serde_json::to_value(
            read_publication_lookup(&state, &scope, job.target_id, None)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            empty,
            json!({"target_id":job.target_id,"attempt_id":null,"job":null,"observations":[],"next_before":null})
        );
        channels
            .claim(&scope, job.target_id, job.attempt_id, Utc::now())
            .await
            .unwrap();
        let unscheduled = serde_json::to_value(
            read_publication_lookup(&state, &scope, job.target_id, None)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(unscheduled["attempt_id"], job.attempt_id.to_string());
        assert!(unscheduled["job"].is_null());
        let at = Utc::now();
        let candidate = job.candidate_public_url.as_ref().unwrap();
        let valid_evidence = json!({
            "schema_version":"geo.publication.asset_observation.v1",
            "provenance":"live",
            "original_attempt_id":job.attempt_id,
            "target_id":job.target_id,
            "account_id":job.account_id,
            "connector_version":"zhihu.v1",
            "content_sha256":sha256_hex(b"Original title\nOriginal body"),
            "observed_at":at,
            "public_url":candidate,
            "secret_field":"never expose me"
        });
        let history: Vec<_> = (0..23).map(|number| PublicationLookupObservation {
            execution_id: Uuid::from_u128(100 + number),
            attempt_id: job.attempt_id,
            finding: PublicationLookupFinding::AssetObserved,
            evidence: if number == 1 { json!({"public_url":"https://www.zhihu.com/p/999","schema_version":"geo.publication.asset_observation.v1","provenance":"live"}) } else {valid_evidence.clone()},
            observed_at: at,
            received_at: at,
            error_code: (number == 0).then(|| "secret_error_detail".into()),
        }).collect();
        let state = state.with_publication_lookup_repository(Arc::new(ReadOnlyLookup {
            job: PublicationLookupJob {
                last_error_code: Some("secret_job_detail".into()),
                lease_expires_at: Some(at + Duration::minutes(1)),
                ..job.clone()
            },
            history,
        }));
        let first = serde_json::to_value(
            read_publication_lookup(&state, &scope, job.target_id, None)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(first["observations"].as_array().unwrap().len(), 20);
        assert_eq!(first["next_before"], Uuid::from_u128(119).to_string());
        assert_eq!(first["job"]["last_error_code"], "lookup_error");
        assert_eq!(first["job"]["in_progress"], true);
        assert_eq!(first["observations"][0]["error_code"], "lookup_error");
        assert!(first["observations"][1]["public_url"].is_null());
        assert_eq!(first["observations"][2]["public_url"], candidate.as_str());
        let serialized = first.to_string();
        for forbidden in [
            "secret_field",
            "secret_error_detail",
            "secret_job_detail",
            "frozen_input",
            "account_id",
            "lease_execution_id",
            "evidence",
            "content_sha256",
        ] {
            assert!(!serialized.contains(forbidden), "leaked {forbidden}");
        }
        assert_eq!(first["job"].as_object().unwrap().len(), 4);
        let next = serde_json::to_value(
            read_publication_lookup(&state, &scope, job.target_id, Some(Uuid::from_u128(119)))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(next["observations"].as_array().unwrap().len(), 3);
        assert!(next["next_before"].is_null());
        assert_eq!(
            read_publication_lookup(&state, &scope, job.target_id, Some(Uuid::new_v4()))
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidRequest
        );
    }

    #[tokio::test]
    async fn only_bound_sanitized_live_observation_exposes_public_url() {
        let state = AppState::development();
        let scope = TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        );
        let job = job();
        let at = Utc::now();
        let evidence = json!({
            "schema_version":"geo.publication.asset_observation.v1",
            "provenance":"live",
            "original_attempt_id":job.attempt_id,
            "target_id":job.target_id,
            "account_id":job.account_id,
            "connector_version":job.connector_version,
            "content_sha256":sha256_hex(b"Original title\nOriginal body"),
            "observed_at":at,
            "public_url":job.candidate_public_url,
        });
        let mut observation = PublicationLookupObservation {
            execution_id: Uuid::new_v4(),
            attempt_id: job.attempt_id,
            finding: PublicationLookupFinding::AssetObserved,
            evidence,
            observed_at: DateTime::from_timestamp_micros(at.timestamp_micros()).unwrap(),
            received_at: at,
            error_code: None,
        };
        assert_eq!(
            public_observed_url(&state, &scope, &observation, &job).await,
            job.candidate_public_url
        );
        for (key, value) in [
            ("content_sha256", json!("wrong")),
            ("public_url", json!("https://www.zhihu.com/p/999")),
            ("provenance", json!("fixture")),
            ("original_attempt_id", json!(Uuid::new_v4())),
            ("observed_at", json!(at - Duration::minutes(1))),
        ] {
            let original = observation.evidence[key].clone();
            observation.evidence[key] = value;
            assert!(
                public_observed_url(&state, &scope, &observation, &job)
                    .await
                    .is_none(),
                "accepted altered {key}"
            );
            observation.evidence[key] = original;
        }
        let mut forged_discovery = observation;
        forged_discovery.evidence["schema_version"] = json!("geo.publication.asset_observation.v2");
        forged_discovery.evidence["discovery_kind"] = json!("own_account_list_discovery");
        forged_discovery.evidence["version_authority"] = json!("presend_encrypted_binding");
        assert!(
            public_observed_url(&state, &scope, &forged_discovery, &job)
                .await
                .is_none(),
            "a marker without the original encrypted binding is not authoritative"
        );
    }

    #[derive(Clone, Default)]
    struct Stub {
        requests: Arc<Mutex<Vec<Value>>>,
        identity: Arc<Mutex<String>>,
    }

    async fn stub_runner(
        State(stub): State<Stub>,
        method: Method,
        uri: Uri,
        body: axum::body::Bytes,
    ) -> (StatusCode, Json<Value>) {
        let payload: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let result = match (method.as_str(), uri.path()) {
            ("GET", "/v1/capabilities") => json!({"connectors":[{
                "platform":"zhihu","placement_slot":"primary","connector_version":"zhihu.v1",
                "operations":["publish","lookup"],"verified":false
            }]}),
            ("POST", "/v1/sessions") => json!({"session_id":payload["session_id"]}),
            ("POST", "/v1/executions") => {
                stub.requests.lock().await.push(payload.clone());
                let discovery = payload["payload"].get("public_url").is_none();
                let url = if discovery {
                    json!("https://www.zhihu.com/p/12345")
                } else {
                    payload["payload"]["public_url"].clone()
                };
                let digest = sha256_hex(b"Original title\nOriginal body");
                let at = Utc::now();
                let mut evidence = vec![json!({"kind":"public_readback","url":url,
                    "content_matched":true,"owned_by_account":true,
                    "expected_sha256":digest,"readback_sha256":digest})];
                if discovery {
                    evidence.push(json!({"kind":"own_account_list_discovery",
                        "schema_version":"geo.publication.discovery.v1","public_url":url,
                        "expected_sha256":digest,"list_complete":true,"exact_match_count":1,
                        "observed_at":at}));
                }
                json!({
                    "execution_id":payload["execution_id"],"status":"completed",
                    "stage":"public_readback","provenance":"live","connector_version":"zhihu.v1",
                    "occurred_at":at,"public_url":url,"evidence":evidence
                })
            }
            ("POST", path) if path.ends_with("/complete") => json!({
                "identity":{"platform_account_id":stub.identity.lock().await.clone(),"display_name":"Test"},
                "storage_state":{"cookies":[],"origins":[]}
            }),
            _ => json!({"closed":true}),
        };
        (StatusCode::OK, Json(result))
    }

    #[tokio::test]
    async fn bound_lookup_never_sends_or_rewrites_original_and_releases_account() {
        use geo_domain::{
            ChannelAccount, ChannelAccountRecord, ChannelOutcome, ChannelOutcomeStatus,
            ChannelOwnerKind, ChannelPlan, ChannelSecret, ChannelStatus, ChannelTarget,
            MemoryChannelRepository,
        };
        let stub = Stub {
            identity: Arc::new(Mutex::new("account-identity".into())),
            ..Stub::default()
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .fallback(any(stub_runner))
            .with_state(stub.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let key = "ac".repeat(32);
        let bridge =
            crate::BrowserBridge::new(format!("http://{address}"), "test-runner".into()).unwrap();
        let service = crate::ChannelService::persistent(
            Arc::new(MemoryChannelRepository::default()),
            &key,
            Some(bridge),
        )
        .unwrap();
        let state = AppState::development().with_channel_service(service);
        let job = job();
        let scope = TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        );
        let aad = format!(
            "geo-channel-v1:{}:{}:{}:{}:session",
            scope.operator_id,
            scope.tenant_id,
            scope.project_id.unwrap(),
            job.account_id
        );
        let encrypted = geo_provider::SecretEnvelope::from_hex_key(&key)
            .unwrap()
            .seal(aad.as_bytes(), br#"{"cookies":[],"origins":[]}"#)
            .unwrap();
        state
            .channel_service()
            .repository
            .save_account(
                &scope,
                ChannelAccountRecord {
                    account: ChannelAccount {
                        account_id: job.account_id,
                        project_id: scope.project_id.unwrap(),
                        owner_kind: ChannelOwnerKind::Customer,
                        platform: "zhihu".into(),
                        group_id: None,
                        status: ChannelStatus::Ready,
                        display_name: None,
                        platform_account_id: Some("account-identity".into()),
                        avatar_url: None,
                        enabled: true,
                        proxy_configured: false,
                        proxy_server: None,
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                    },
                    session: Some(ChannelSecret::new(encrypted)),
                    proxy: None,
                },
            )
            .await
            .unwrap();
        let jobs = state.channel_job_repository();
        jobs.create_plan(
            &scope,
            ChannelPlan {
                plan_id: Uuid::new_v4(),
                project_id: scope.project_id.unwrap(),
                cycle_id: Uuid::new_v4(),
                input_hash: "frozen".into(),
                revision: 1,
                created_at: Utc::now(),
                targets: vec![ChannelTarget {
                    target_id: job.target_id,
                    input: job.frozen_input.clone(),
                }],
            },
        )
        .await
        .unwrap();
        jobs.claim(&scope, job.target_id, job.attempt_id, Utc::now())
            .await
            .unwrap();
        let (session, binding) = state
            .channel_service()
            .resume_available_browser_bound(&scope, job.account_id, job.attempt_id)
            .await
            .unwrap();
        state
            .channel_service()
            .browser
            .as_ref()
            .unwrap()
            .close(session)
            .await
            .unwrap();
        jobs.store_publication_binding(&scope, job.target_id, job.attempt_id, binding)
            .await
            .unwrap();
        let mut no_receipt = job.clone();
        no_receipt.connector_version = None;
        no_receipt.candidate_public_url = None;
        let (discovered, evidence, error) =
            lookup_once(&state, &scope, &no_receipt, Uuid::new_v4(), Utc::now()).await;
        assert_eq!(discovered, PublicationLookupFinding::AssetObserved);
        assert_eq!(error, None);
        assert_eq!(
            evidence["schema_version"],
            "geo.publication.asset_observation.v2"
        );
        assert_eq!(evidence["connector_version"], "zhihu.v1");
        assert!(
            stub.requests.lock().await[0]["payload"]
                .get("public_url")
                .is_none()
        );
        let at = evidence["observed_at"]
            .as_str()
            .unwrap()
            .parse::<DateTime<Utc>>()
            .unwrap();
        let observation = PublicationLookupObservation {
            execution_id: Uuid::new_v4(),
            attempt_id: job.attempt_id,
            finding: discovered,
            evidence: evidence.clone(),
            observed_at: at,
            received_at: Utc::now(),
            error_code: None,
        };
        assert_eq!(
            public_observed_url(&state, &scope, &observation, &no_receipt).await,
            Some("https://www.zhihu.com/p/12345".into())
        );
        let mut corrupted = observation.clone();
        corrupted.evidence["version_authority"] = json!("live_capability");
        assert!(
            public_observed_url(&state, &scope, &corrupted, &no_receipt)
                .await
                .is_none()
        );
        corrupted.evidence = evidence;
        corrupted.evidence["content_sha256"] = json!("other");
        assert!(
            public_observed_url(&state, &scope, &corrupted, &no_receipt)
                .await
                .is_none()
        );
        let original = ChannelOutcome {
            status: ChannelOutcomeStatus::Unknown,
            detail: None,
            occurred_at: Utc::now(),
            raw_answer: None,
            citations: vec![],
            public_url: None,
            screenshot_ref: None,
            connector_version: Some("zhihu.v1".into()),
            runner_evidence: vec![],
            fixture: true,
        };
        let before = jobs
            .finish(&scope, job.target_id, job.attempt_id, original, Utc::now())
            .await
            .unwrap();
        // A late unknown receipt may once-fill the hint and version; that
        // must not erase the earlier independently bound observation.
        assert_eq!(
            public_observed_url(&state, &scope, &observation, &job).await,
            Some("https://www.zhihu.com/p/12345".into())
        );
        let mut different_late_hint = job.clone();
        different_late_hint.candidate_public_url = Some("https://www.zhihu.com/p/999".into());
        assert_eq!(
            public_observed_url(&state, &scope, &observation, &different_late_hint).await,
            Some("https://www.zhihu.com/p/12345".into())
        );
        let read_state =
            state
                .clone()
                .with_publication_lookup_repository(Arc::new(ReadOnlyLookup {
                    job: job.clone(),
                    history: vec![observation.clone()],
                }));
        let projection = read_publication_lookup(&read_state, &scope, job.target_id, None)
            .await
            .unwrap();
        assert_eq!(
            projection.observations[0].public_url.as_deref(),
            Some("https://www.zhihu.com/p/12345")
        );
        let mut future_observation = observation;
        future_observation.received_at = future_observation.observed_at - Duration::seconds(1);
        assert!(
            public_observed_url(&state, &scope, &future_observation, &job)
                .await
                .is_none()
        );
        let result = lookup_once(&state, &scope, &job, Uuid::new_v4(), Utc::now()).await;
        assert_eq!(result.0, PublicationLookupFinding::AssetObserved);
        assert!(result.2.is_none());
        assert_eq!(stub.requests.lock().await.len(), 2);
        assert_eq!(stub.requests.lock().await[1]["operation"], "lookup");
        assert_eq!(
            jobs.get_target(&scope, job.target_id).await.unwrap(),
            before
        );
        // Wrong live identity prevents the operation and still closes/releases.
        *stub.identity.lock().await = "other-identity".into();
        let refused = lookup_once(&state, &scope, &job, Uuid::new_v4(), Utc::now()).await;
        assert_eq!(refused.0, PublicationLookupFinding::Unknown);
        assert_eq!(stub.requests.lock().await.len(), 2);
        let reservation = Uuid::new_v4();
        let reserve_at = Utc::now();
        jobs.reserve_account(
            &scope,
            job.account_id,
            reservation,
            reserve_at,
            reserve_at + LEASE,
        )
        .await
        .unwrap();
        jobs.release_account(&scope, job.account_id, reservation)
            .await
            .unwrap();
        assert_eq!(
            jobs.get_target(&scope, job.target_id).await.unwrap(),
            before
        );
        server.abort();
    }

    fn job() -> PublicationLookupJob {
        let account_id = Uuid::new_v4();
        PublicationLookupJob {
            attempt_id: Uuid::new_v4(),
            target_id: Uuid::new_v4(),
            account_id,
            frozen_input: ChannelTargetInput::Publish {
                source_id: Uuid::new_v4(),
                source_version_id: Uuid::new_v4(),
                platform: "zhihu".into(),
                account_id,
                title: "Original title".into(),
                body: "Original body".into(),
                body_sha256: sha256_hex(b"Original body"),
            },
            connector_version: Some("zhihu.v1".into()),
            candidate_public_url: Some("https://www.zhihu.com/p/12345".into()),
            next_due_at: Some(Utc::now()),
            lease_execution_id: None,
            lease_expires_at: None,
            query_count: 0,
            last_error_code: None,
        }
    }

    #[tokio::test]
    async fn legacy_text_lookup_never_projects_rich_as_public_verification() {
        let state = AppState::development();
        let scope = TenantScope::new(
            OperatorId::new(Uuid::new_v4()),
            TenantId::new(Uuid::new_v4()),
            Some(ProjectId::new(Uuid::new_v4())),
        );
        let mut job = job();
        job.frozen_input = ChannelTargetInput::GeneratedPublish {
            content_revision_id: Uuid::new_v4(),
            variant_id: Uuid::new_v4(),
            publication_intent_id: Uuid::new_v4(),
            distribution_target_id: Uuid::nil(),
            origin_request_id: Some(Uuid::new_v4()),
            platform: "zhihu".into(),
            account_id: job.account_id,
            title: "Original title".into(),
            body: "Original body".into(),
            body_sha256: sha256_hex(b"Original body"),
            payload_hash: "rich-frozen".into(),
            evidence: vec![],
            rich_payload: Some(geo_domain::RichPublicationPayload {
                schema_version: 2,
                format: geo_domain::RICH_MARKDOWN_FORMAT.into(),
                content_revision_id: Uuid::new_v4(),
                policy_version: geo_domain::RICH_CHANNEL_VARIANT_POLICY.into(),
                document: geo_domain::StructuredDocument {
                    title: "Original title".into(),
                    blocks: vec![],
                    schema_version: Some(2),
                },
                media: vec![],
            }),
        };
        let observed_at = Utc::now();
        let observation = PublicationLookupObservation {
            execution_id: Uuid::new_v4(),
            attempt_id: job.attempt_id,
            finding: PublicationLookupFinding::AssetObserved,
            evidence: json!({
                "schema_version":"geo.publication.asset_observation.v1",
                "provenance":"live",
                "original_attempt_id":job.attempt_id,
                "target_id":job.target_id,
                "account_id":job.account_id,
                "connector_version":job.connector_version,
                "content_sha256":sha256_hex(b"Original title\nOriginal body"),
                "observed_at":observed_at,
                "public_url":job.candidate_public_url,
            }),
            observed_at,
            received_at: observed_at,
            error_code: None,
        };
        assert!(
            public_observed_url(&state, &scope, &observation, &job)
                .await
                .is_none()
        );
        let (finding, _, code) =
            lookup_once(&state, &scope, &job, Uuid::new_v4(), Utc::now()).await;
        assert_eq!(finding, PublicationLookupFinding::Unknown);
        assert_eq!(code, Some("lookup_unavailable"));
    }

    #[test]
    fn candidate_is_only_the_fixed_public_asset_path() {
        assert!(valid_candidate("https://www.zhihu.com/p/123", "zhihu"));
        for candidate in [
            "https://www.zhihu.com/p/123?token=secret",
            "https://www.zhihu.com/p/123/",
            "https://www.zhihu.com@evil.invalid/p/123",
            "http://www.zhihu.com/p/123",
            "https://www.zhihu.com/p/１２３",
        ] {
            assert!(!valid_candidate(candidate, "zhihu"));
        }
    }

    #[tokio::test]
    async fn missing_candidate_requires_original_binding_before_discovery() {
        let state = AppState::development();
        let scope = TenantScope::new(
            OperatorId::new(Uuid::new_v4()),
            TenantId::new(Uuid::new_v4()),
            Some(ProjectId::new(Uuid::new_v4())),
        );
        let mut job = job();
        job.candidate_public_url = None;
        let (finding, _, code) =
            lookup_once(&state, &scope, &job, Uuid::new_v4(), Utc::now()).await;
        assert_eq!(finding, PublicationLookupFinding::Unknown);
        assert_eq!(code, Some("binding_unavailable"));
    }

    #[tokio::test]
    async fn missing_original_binding_never_opens_a_browser() {
        let state = AppState::development();
        let scope = TenantScope::new(
            OperatorId::new(Uuid::new_v4()),
            TenantId::new(Uuid::new_v4()),
            Some(ProjectId::new(Uuid::new_v4())),
        );
        let mut job = job();
        if let ChannelTargetInput::Publish { account_id, .. } = &mut job.frozen_input {
            *account_id = job.account_id;
        }
        let (finding, _, code) =
            lookup_once(&state, &scope, &job, Uuid::new_v4(), Utc::now()).await;
        assert_eq!(finding, PublicationLookupFinding::Unknown);
        assert_eq!(code, Some("binding_unavailable"));
    }

    #[test]
    fn only_exact_live_frozen_readback_can_observe_an_asset() {
        let mut job = job();
        if let ChannelTargetInput::Publish { account_id, .. } = &mut job.frozen_input {
            *account_id = job.account_id;
        }
        let now = Utc::now();
        let execution_id = Uuid::new_v4();
        let url = job.candidate_public_url.as_deref().unwrap();
        let digest = sha256_hex(b"Original title\nOriginal body");
        let valid = || BrowserExecution {
            execution_id,
            provenance: Some(BrowserReceiptProvenance::Live),
            status: "completed".into(),
            reason: None,
            evidence: vec![json!({
                "kind": "public_readback",
                "url": url,
                "content_matched": true,
                "owned_by_account": true,
                "expected_sha256": digest,
                "readback_sha256": digest,
                "unbounded_untrusted_text": "DO NOT PERSIST",
            })],
            public_url: Some(url.into()),
            occurred_at: Some(now),
            connector_version: Some("zhihu.v1".into()),
            stage: Some("public_readback".into()),
        };
        let evidence = observed_asset(
            &valid(),
            &job,
            "zhihu.v1",
            execution_id,
            now - Duration::seconds(1),
            now + Duration::seconds(1),
        )
        .expect("live readback");
        assert_eq!(evidence["public_url"], url);
        assert!(!evidence.to_string().contains("DO NOT PERSIST"));
        let mut fixture = valid();
        fixture.provenance = Some(BrowserReceiptProvenance::Fixture);
        assert!(observed_asset(&fixture, &job, "zhihu.v1", execution_id, now, now).is_none());
        let mut other_execution = valid();
        other_execution.execution_id = Uuid::new_v4();
        assert!(
            observed_asset(&other_execution, &job, "zhihu.v1", execution_id, now, now).is_none()
        );
        let mut other_version = valid();
        other_version.connector_version = Some("zhihu.v2".into());
        assert!(observed_asset(&other_version, &job, "zhihu.v1", execution_id, now, now).is_none());
        let mut other_asset = valid();
        other_asset.public_url = Some("https://www.zhihu.com/p/999".into());
        assert!(observed_asset(&other_asset, &job, "zhihu.v1", execution_id, now, now).is_none());
        let mut spoof = valid();
        spoof.evidence.push(json!({"kind":"runner_receipt"}));
        assert!(observed_asset(&spoof, &job, "zhihu.v1", execution_id, now, now).is_none());
        let mut stale = valid();
        stale.occurred_at = Some(now - Duration::seconds(2));
        assert!(observed_asset(&stale, &job, "zhihu.v1", execution_id, now, now).is_none());
        let mut original_changed = job.clone();
        if let ChannelTargetInput::Publish { title, .. } = &mut original_changed.frozen_input {
            *title = "Altered title".into();
        }
        assert!(
            observed_asset(
                &valid(),
                &original_changed,
                "zhihu.v1",
                execution_id,
                now,
                now
            )
            .is_none()
        );
    }

    #[test]
    fn discovery_requires_one_exact_complete_list_proof_and_matching_readback() {
        let mut job = job();
        job.candidate_public_url = None;
        job.connector_version = None;
        let at = Utc::now();
        let execution_id = Uuid::new_v4();
        let url = "https://www.zhihu.com/p/12345";
        let digest = sha256_hex(b"Original title\nOriginal body");
        let valid = || BrowserExecution {
            execution_id,
            provenance: Some(BrowserReceiptProvenance::Live),
            status: "completed".into(),
            reason: None,
            evidence: vec![
                json!({"kind":"public_readback","url":url,"content_matched":true,
                    "owned_by_account":true,"expected_sha256":digest,"readback_sha256":digest}),
                json!({"kind":"own_account_list_discovery",
                    "schema_version":"geo.publication.discovery.v1","public_url":url,
                    "expected_sha256":digest,"list_complete":true,"exact_match_count":1,
                    "observed_at":at}),
            ],
            public_url: Some(url.into()),
            occurred_at: Some(at),
            connector_version: Some("zhihu.v1".into()),
            stage: Some("public_readback".into()),
        };
        let accepts = |receipt: &BrowserExecution| {
            observed_asset(receipt, &job, "zhihu.v1", execution_id, at, at).is_some()
        };
        assert!(accepts(&valid()));
        assert!(!accepts(&BrowserExecution {
            provenance: Some(BrowserReceiptProvenance::Fixture),
            ..valid()
        }));
        assert!(!accepts(&BrowserExecution {
            connector_version: Some("zhihu.v2".into()),
            ..valid()
        }));
        for (key, value) in [
            ("list_complete", json!(false)),
            ("exact_match_count", json!(2)),
            ("exact_match_count", json!(0)),
            ("expected_sha256", json!("wrong")),
            ("public_url", json!("https://www.zhihu.com/p/999")),
            ("observed_at", json!(at - Duration::seconds(1))),
            ("schema_version", json!("other")),
        ] {
            let mut wrong = valid();
            wrong.evidence[1][key] = value;
            assert!(!accepts(&wrong), "accepted altered discovery {key}");
        }
        let mut extra = valid();
        extra.evidence[1]["unexpected"] = json!("extra");
        assert!(!accepts(&extra));
        let mut repeated = valid();
        repeated.evidence.push(repeated.evidence[1].clone());
        assert!(!accepts(&repeated));
        let mut wrong_readback = valid();
        wrong_readback.evidence[0]["owned_by_account"] = json!(false);
        assert!(!accepts(&wrong_readback));
        let mut wrong_identity = job.clone();
        wrong_identity.account_id = Uuid::new_v4();
        assert!(
            observed_asset(&valid(), &wrong_identity, "zhihu.v1", execution_id, at, at).is_none()
        );
    }
}
