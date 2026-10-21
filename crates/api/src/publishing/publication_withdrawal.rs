//! Once-only cleanup of the exact short post created by an opted-in request.
use axum::{
    Json,
    extract::{Extension, Path, State},
};
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, ChannelTargetInput, ErrorCode, ProjectId, PublicationWithdrawal,
    PublicationWithdrawalState, TenantScope,
};
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    ApiError, AppState, RequestContext, api_error,
    browser_bridge::{BrowserExecution, BrowserReceiptProvenance},
    channel_jobs::execute_and_close_with_cleanup,
};

#[derive(Serialize)]
pub struct WithdrawalRead {
    request_id: Uuid,
    target_id: Uuid,
    attempt_id: Uuid,
    state: PublicationWithdrawalState,
    public_url: Option<String>,
    error_code: Option<&'static str>,
}

pub(crate) async fn get_withdrawal(
    State(state): State<AppState>,
    Path((project_id, request_id)): Path<(ProjectId, Uuid)>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<Option<WithdrawalRead>>, ApiError> {
    let result = async {
        let scope = crate::content::scoped(&state, &tenant, project_id).await?;
        let publication =
            crate::content_distribution_requests::read_content_distribution_publication(
                &state, &scope, request_id,
            )
            .await?;
        let Some(attempt_id) = publication.attempt_id else {
            return Ok(None);
        };
        let row = match state
            .channel_job_repository()
            .get_withdrawal(&scope, attempt_id)
            .await
        {
            Ok(row) => row,
            Err(error) if error.code == ErrorCode::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(Some(WithdrawalRead {
            request_id,
            target_id: row.target_id,
            attempt_id,
            state: row.state,
            public_url: row.public_url,
            error_code: row.error_code.as_ref().map(|_| "withdrawal_unverified"),
        }))
    }
    .await;
    result
        .map(Json)
        .map_err(|error| api_error(error, context.request_id))
}

/// Called by the existing channel dispatcher. Only identities from the durable
/// scan can reach this entry; account, body and deletion URL are never inputs.
pub async fn execute_publication_withdrawal(
    state: &AppState,
    row: PublicationWithdrawal,
) -> Result<(), AppError> {
    let repository = state.channel_job_repository();
    let scope = &row.scope;
    let row = repository.resolve_withdrawal(scope, row.attempt_id).await?;
    if row.state != PublicationWithdrawalState::Ready {
        return Ok(());
    }
    let target = repository.get_target(scope, row.target_id).await?.target;
    let ChannelTargetInput::GeneratedPublish {
        body,
        rich_payload: None,
        origin_request_id: Some(request_id),
        placement_slot,
        ..
    } = &target.input
    else {
        return Err(AppError::conflict("withdrawal target differs"));
    };
    if *request_id != row.request_id
        || placement_slot != "short_post"
        || target.input.account_id() != row.account_id
    {
        return Err(AppError::conflict("withdrawal target differs"));
    }
    let Some(binding) = repository
        .get_publication_binding(scope, row.target_id, row.attempt_id)
        .await?
    else {
        return Ok(());
    };
    let Some(bridge) = state.channel_service().browser.as_ref() else {
        return Ok(());
    };
    let now = Utc::now();
    let reservation = Uuid::new_v4();
    if let Err(error) = repository
        .reserve_account(
            scope,
            row.account_id,
            reservation,
            now,
            now + geo_domain::CHANNEL_EXECUTION_LEASE,
        )
        .await
    {
        return if error.code == ErrorCode::Conflict {
            Ok(())
        } else {
            Err(error)
        };
    }
    let session_id = Uuid::new_v4();
    let opened = state
        .channel_service()
        .resume_publication_withdrawal_browser_reserved(
            scope,
            row.account_id,
            row.attempt_id,
            &binding,
            session_id,
            reservation,
        )
        .await;
    let (session, identity, version) = match opened {
        Ok(value) => value,
        Err(error) => {
            let _ = state
                .channel_service()
                .close_owned_browser(scope, session_id)
                .await;
            let _ = repository
                .release_account(scope, row.account_id, reservation)
                .await;
            return Err(error);
        }
    };
    if row.evidence["connector_version"].as_str() != Some(version.as_str())
        || Utc::now() + chrono::Duration::minutes(3) >= now + geo_domain::CHANNEL_EXECUTION_LEASE
    {
        let _ = state
            .channel_service()
            .close_owned_browser(scope, session)
            .await;
        let _ = repository
            .release_account(scope, row.account_id, reservation)
            .await;
        return Err(AppError::conflict(
            "withdrawal preflight expired or changed",
        ));
    }
    let execution_id = Uuid::new_v4();
    let claimed = repository
        .claim_withdrawal(scope, row.attempt_id, execution_id, reservation, Utc::now())
        .await;
    let claimed = match claimed {
        Ok(row) => row,
        Err(error) => {
            let _ = state
                .channel_service()
                .close_owned_browser(scope, session)
                .await;
            let _ = repository
                .release_account(scope, row.account_id, reservation)
                .await;
            return if error.code == ErrorCode::Conflict {
                Ok(())
            } else {
                Err(error)
            };
        }
    };
    let payload =
        json!({"placement_slot":"short_post", "external_id":claimed.external_id, "body":body});
    let (receipt, closed) = execute_and_close_with_cleanup(
        bridge,
        state.channel_service(),
        scope,
        session,
        Some(&identity),
        execution_id,
        "withdraw",
        &payload,
    )
    .await;
    if closed {
        let _ = repository
            .release_account(scope, row.account_id, reservation)
            .await;
    }
    let received_at = Utc::now();
    let evidence = receipt
        .ok()
        .and_then(|receipt| {
            withdrawal_evidence(&receipt, &claimed, &version, execution_id, received_at)
        })
        .unwrap_or_else(|| json!({}));
    repository
        .finish_withdrawal(scope, row.attempt_id, execution_id, evidence, received_at)
        .await?;
    Ok(())
}

fn withdrawal_evidence(
    receipt: &BrowserExecution,
    row: &PublicationWithdrawal,
    version: &str,
    execution_id: Uuid,
    received_at: DateTime<Utc>,
) -> Option<Value> {
    let at = receipt.occurred_at?;
    if receipt.execution_id != execution_id
        || receipt.provenance != Some(BrowserReceiptProvenance::Live)
        || receipt.status != "completed"
        || receipt.stage.as_deref() != Some("withdraw")
        || receipt.connector_version.as_deref() != Some(version)
        || receipt.public_url != row.public_url
        || row.claimed_at.is_none_or(|claimed| at < claimed)
        || at > received_at
        || receipt.evidence.len() != 3
    {
        return None;
    }
    let one = |kind: &str| {
        let mut values = receipt.evidence.iter().filter(|p| p["kind"] == kind);
        let value = values.next()?;
        values.next().is_none().then_some(value)
    };
    let attempt = one("withdrawal_attempt")?;
    let response = one("withdrawal_response")?;
    let absence = one("public_absence")?;
    let id = row.external_id.as_deref()?;
    let url = row.public_url.as_deref()?;
    for proof in [attempt, response, absence] {
        let observed = proof["observed_at"]
            .as_str()?
            .parse::<DateTime<Utc>>()
            .ok()?;
        if proof["external_id"] != id || observed < row.claimed_at? || observed > at {
            return None;
        }
    }
    let attempt_at = attempt["observed_at"]
        .as_str()?
        .parse::<DateTime<Utc>>()
        .ok()?;
    let response_at = response["observed_at"]
        .as_str()?
        .parse::<DateTime<Utc>>()
        .ok()?;
    let absence_at = absence["observed_at"]
        .as_str()?
        .parse::<DateTime<Utc>>()
        .ok()?;
    if attempt["url"] != url
        || absence["url"] != url
        || response["url"] != format!("https://www.zhihu.com/api/v4/pins/{id}")
        || attempt["placement_slot"] != "short_post"
        || attempt["expected_sha256"] != row.evidence["expected_sha256"]
        || attempt["delete_clicked"] != true
        || attempt["confirmation_clicked"] != true
        || attempt["withdrawal_verified"] != true
        || response["method"] != "DELETE"
        || !response["http_status"]
            .as_u64()
            .is_some_and(|s| (200..300).contains(&s))
        || absence["absence_verified"] != true
        || !absence["http_status"]
            .as_u64()
            .is_some_and(|s| [404, 410].contains(&s))
        || attempt_at > response_at
        || response_at > absence_at
    {
        return None;
    }
    Some(
        json!({"schema_version":"geo.publication.withdrawal.v1", "execution_id":execution_id,
        "external_id":id,"public_url":url,"expected_sha256":row.evidence["expected_sha256"],
        "connector_version":version,"provenance":"live","delete_confirmed":true,
        "delete_status":response["http_status"],"absence_status":absence["http_status"],"observed_at":at}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn withdrawal_requires_correlated_delete_and_public_absence() {
        let now = Utc::now();
        let execution_id = Uuid::new_v4();
        let digest =
            geo_domain::plain_text_publication_readback_hash("short_post", "", "Synthetic body");
        let mut row = PublicationWithdrawal {
            scope: TenantScope::new(
                Uuid::new_v4().into(),
                Uuid::new_v4().into(),
                Some(Uuid::new_v4().into()),
            ),
            request_id: Uuid::new_v4(),
            target_id: Uuid::new_v4(),
            attempt_id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            state: PublicationWithdrawalState::Claimed,
            external_id: Some("42".into()),
            public_url: Some("https://www.zhihu.com/pin/42".into()),
            execution_id: Some(execution_id),
            claimed_at: Some(now),
            evidence: json!({"expected_sha256":digest,"connector_version":"synthetic.v1"}),
            finished_at: None,
            error_code: None,
        };
        let receipt_json = json!({"execution_id":execution_id,"provenance":"live","status":"completed","stage":"withdraw",
        "connector_version":"synthetic.v1","public_url":row.public_url,"occurred_at":now,
        "evidence":[
            {"kind":"withdrawal_attempt","external_id":"42","url":row.public_url,"placement_slot":"short_post",
             "expected_sha256":digest,"observed_at":now,"delete_clicked":true,"confirmation_clicked":true,"withdrawal_verified":true},
            {"kind":"withdrawal_response","external_id":"42","url":"https://www.zhihu.com/api/v4/pins/42","method":"DELETE","http_status":204,"observed_at":now},
            {"kind":"public_absence","external_id":"42","url":row.public_url,"http_status":404,"absence_verified":true,"observed_at":now}
        ]});
        let receipt: BrowserExecution = serde_json::from_value(receipt_json.clone()).unwrap();
        let evidence =
            withdrawal_evidence(&receipt, &row, "synthetic.v1", execution_id, now).unwrap();
        geo_domain::finish_publication_withdrawal(&mut row, execution_id, evidence, now).unwrap();
        assert_eq!(row.state, PublicationWithdrawalState::Withdrawn);
        row.state = PublicationWithdrawalState::Claimed;
        for (index, field, value) in [
            (0, "confirmation_clicked", json!(false)),
            (0, "expected_sha256", json!("unrelated")),
            (1, "external_id", json!("43")),
            (1, "method", json!("GET")),
            (1, "http_status", json!(403)),
            (2, "url", json!("https://www.zhihu.com/pin/43")),
            (2, "absence_verified", json!(false)),
            (2, "http_status", json!(200)),
        ] {
            let mut bad = receipt_json.clone();
            bad["evidence"][index][field] = value;
            let receipt = serde_json::from_value(bad).unwrap();
            assert!(
                withdrawal_evidence(&receipt, &row, "synthetic.v1", execution_id, now).is_none(),
                "{index}:{field}"
            );
        }
        let mut fixture = receipt_json;
        fixture["provenance"] = json!("fixture");
        assert!(
            withdrawal_evidence(
                &serde_json::from_value(fixture).unwrap(),
                &row,
                "synthetic.v1",
                execution_id,
                now
            )
            .is_none()
        );
    }

    #[test]
    fn withdrawal_choice_must_be_explicit_in_acceptance_dto() {
        let mut input = json!({"content_asset_id":Uuid::new_v4(),"content_revision_id":Uuid::new_v4(),"account_id":Uuid::new_v4(),
            "placement_slot":"short_post","format":"markdown.v1"});
        assert!(
            serde_json::from_value::<crate::SingleArticleDistributionRequest>(input.clone())
                .is_err()
        );
        input["withdraw_after_publish"] = json!(false);
        assert!(serde_json::from_value::<crate::SingleArticleDistributionRequest>(input).is_ok());
    }
}
