//! A claimed lookup may observe a public asset, but cannot certify the
//! original send or mutate its immutable publication receipt.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use geo_domain::{
    AppError, ChannelTargetInput, ErrorCode, PublicationLookupFinding, PublicationLookupJob,
    PublicationLookupObservation, PublicationLookupRepository, TenantScope, sha256_hex,
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    AppState,
    browser_bridge::{BrowserExecution, BrowserReceiptProvenance},
    channel_jobs::{execute_and_close_with_cleanup, publication_readback},
};

const LEASE: Duration = Duration::minutes(5);

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
    // Never allow arbitrary URLs from a malformed job or a later caller to
    // become navigation instructions to an authenticated browser context.
    let Some(candidate) = job.candidate_public_url.as_deref() else {
        return unknown("candidate_missing");
    };
    if !valid_candidate(candidate, platform) {
        return unknown("candidate_invalid");
    }
    let Some(original_version) = job.connector_version.as_deref() else {
        return unknown("connector_version_missing");
    };
    if original_version.is_empty() || original_version.len() > 100 {
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
    if original_version != bound_version {
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
    let payload = json!({"title":title,"body":body,"public_url":candidate});
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
            match observed_asset(&receipt, job, execution_id, claimed_at, received_at) {
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
    execution_id: Uuid,
    claimed_at: DateTime<Utc>,
    received_at: DateTime<Utc>,
) -> Option<Value> {
    let at = receipt.occurred_at?;
    let version = job.connector_version.as_deref()?;
    let candidate = job.candidate_public_url.as_deref()?;
    if receipt.execution_id != execution_id
        || receipt.provenance != Some(BrowserReceiptProvenance::Live)
        || receipt.connector_version.as_deref() != Some(version)
        || receipt.public_url.as_deref() != Some(candidate)
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
    let (title, body) = match &job.frozen_input {
        ChannelTargetInput::Publish { title, body, .. }
        | ChannelTargetInput::GeneratedPublish { title, body, .. } => (title, body),
        ChannelTargetInput::Measure { .. } => return None,
    };
    let normalize = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let digest = sha256_hex(format!("{}\n{}", normalize(title), normalize(body)).as_bytes());
    Some(json!({
        "schema_version": "geo.publication.asset_observation.v1",
        "public_url": candidate,
        "content_sha256": digest,
        "connector_version": version,
        "observed_at": at,
        "original_attempt_id": job.attempt_id,
        "target_id": job.target_id,
        "account_id": job.account_id,
        "provenance": "live",
        // Critically, no claim that the original send created this asset.
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::State,
        http::{Method, StatusCode, Uri},
        routing::any,
    };
    use geo_domain::{OperatorId, ProjectId, TenantId};
    use tokio::sync::Mutex;

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
                let url = payload["payload"]["public_url"].clone();
                let digest = sha256_hex(b"Original title\nOriginal body");
                json!({
                    "execution_id":payload["execution_id"],"status":"completed",
                    "stage":"public_readback","provenance":"live","connector_version":"zhihu.v1",
                    "occurred_at":Utc::now(),"public_url":url,
                    "evidence":[{"kind":"public_readback","url":url,
                        "content_matched":true,"owned_by_account":true,
                        "expected_sha256":digest,"readback_sha256":digest}]
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
        let result = lookup_once(&state, &scope, &job, Uuid::new_v4(), Utc::now()).await;
        assert_eq!(result.0, PublicationLookupFinding::AssetObserved);
        assert!(result.2.is_none());
        assert_eq!(stub.requests.lock().await.len(), 1);
        assert_eq!(stub.requests.lock().await[0]["operation"], "lookup");
        assert_eq!(
            jobs.get_target(&scope, job.target_id).await.unwrap(),
            before
        );
        // Wrong live identity prevents the operation and still closes/releases.
        *stub.identity.lock().await = "other-identity".into();
        let refused = lookup_once(&state, &scope, &job, Uuid::new_v4(), Utc::now()).await;
        assert_eq!(refused.0, PublicationLookupFinding::Unknown);
        assert_eq!(stub.requests.lock().await.len(), 1);
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
    async fn missing_candidate_does_not_need_a_runner_or_a_binding() {
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
        assert_eq!(code, Some("candidate_missing"));
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
            execution_id,
            now - Duration::seconds(1),
            now + Duration::seconds(1),
        )
        .expect("live readback");
        assert_eq!(evidence["public_url"], url);
        assert!(!evidence.to_string().contains("DO NOT PERSIST"));
        let mut fixture = valid();
        fixture.provenance = Some(BrowserReceiptProvenance::Fixture);
        assert!(observed_asset(&fixture, &job, execution_id, now, now).is_none());
        let mut other_execution = valid();
        other_execution.execution_id = Uuid::new_v4();
        assert!(observed_asset(&other_execution, &job, execution_id, now, now).is_none());
        let mut other_version = valid();
        other_version.connector_version = Some("zhihu.v2".into());
        assert!(observed_asset(&other_version, &job, execution_id, now, now).is_none());
        let mut other_asset = valid();
        other_asset.public_url = Some("https://www.zhihu.com/p/999".into());
        assert!(observed_asset(&other_asset, &job, execution_id, now, now).is_none());
        let mut spoof = valid();
        spoof.evidence.push(json!({"kind":"runner_receipt"}));
        assert!(observed_asset(&spoof, &job, execution_id, now, now).is_none());
        let mut stale = valid();
        stale.occurred_at = Some(now - Duration::seconds(2));
        assert!(observed_asset(&stale, &job, execution_id, now, now).is_none());
        let mut original_changed = job.clone();
        if let ChannelTargetInput::Publish { title, .. } = &mut original_changed.frozen_input {
            *title = "Altered title".into();
        }
        assert!(observed_asset(&valid(), &original_changed, execution_id, now, now).is_none());
    }
}
