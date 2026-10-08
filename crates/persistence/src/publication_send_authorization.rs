//! Original-attempt rich send authorization. The transaction ends before any
//! external network work; authorization is an irreversible one-shot permission.
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use geo_domain::{
    AppError, AuthorizePublicationSend, ChannelTarget, ChannelTargetInput, ChannelVariant,
    ContentRevision, ErrorCode, IntentVerification, PublicationIntent,
    PublicationSendAuthorizationRepository, PublicationSendDecision, PublicationSendGrant,
    RegisterPublicationSend, TenantScope, sha256_hex, validate_rich_publication_payload,
};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{Database, content_media::validate_content_media_in_transaction, set_local_scope};

/// The preflight is short-lived; the runner must also enforce this deadline
/// immediately before the first external media upload or autosave.
const MAX_GRANT_LIFETIME: Duration = Duration::minutes(5);

#[derive(Clone)]
pub struct PgPublicationSendAuthorizationRepository {
    pool: PgPool,
}

impl PgPublicationSendAuthorizationRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn from_database(database: &Database) -> Self {
        Self::new(database.pool().clone())
    }

    async fn transaction(
        &self,
        scope: &TenantScope,
    ) -> Result<Transaction<'_, Postgres>, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        set_local_scope(&mut tx, scope).await.map_err(db)?;
        Ok(tx)
    }
}

fn db(_error: sqlx::Error) -> AppError {
    // Neither encrypted bindings nor database errors may enter public logs.
    AppError::new(
        ErrorCode::DependencyUnavailable,
        "publication authorization unavailable",
    )
}

fn project(scope: &TenantScope) -> Result<Uuid, AppError> {
    scope
        .project_id
        .map(|id| id.as_uuid())
        .ok_or_else(|| AppError::forbidden("publication requires project scope"))
}

fn decode<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, AppError> {
    serde_json::from_value(value)
        .map_err(|_| AppError::conflict("saved publication input is invalid"))
}

async fn lock_project(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
) -> Result<(), AppError> {
    let status: Option<String> = sqlx::query_scalar(
        "SELECT status FROM projects WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 FOR UPDATE",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project(scope)?)
    .fetch_optional(&mut **tx).await.map_err(db)?;
    match status.as_deref() {
        Some("active") => Ok(()),
        Some(_) => Err(AppError::conflict("project cannot authorize publication")),
        None => Err(AppError::not_found("publication project not found")),
    }
}

async fn locked_target(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    id: Uuid,
) -> Result<ChannelTarget, AppError> {
    let row = sqlx::query(
        "SELECT frozen_input,kind FROM channel_execution_targets
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND target_id=$4 FOR UPDATE",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project(scope)?)
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db)?
    .ok_or_else(|| AppError::not_found("publication target not found"))?;
    let target: ChannelTarget = decode(row.get("frozen_input"))?;
    if row.get::<String, _>("kind") != "publish" || target.target_id != id {
        return Err(AppError::conflict("publication target identity differs"));
    }
    Ok(target)
}

struct Attempt {
    account_id: Uuid,
    claimed_at: DateTime<Utc>,
    outcome: Option<serde_json::Value>,
    runner_session_id: Option<Uuid>,
    send_not_after: Option<DateTime<Utc>>,
    send_authorized_at: Option<DateTime<Utc>>,
}

async fn locked_attempt(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    target_id: Uuid,
    attempt_id: Uuid,
) -> Result<Attempt, AppError> {
    let row = sqlx::query(
        "SELECT account_id,claimed_at,outcome,target_kind,runner_session_id,
                send_not_after,send_authorized_at
         FROM channel_execution_attempts
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3
           AND target_id=$4 AND attempt_id=$5 FOR UPDATE",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project(scope)?)
    .bind(target_id)
    .bind(attempt_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db)?
    .ok_or_else(|| AppError::not_found("publication attempt not found"))?;
    if row.get::<String, _>("target_kind") != "publish" {
        return Err(AppError::conflict("attempt is not a publication"));
    }
    Ok(Attempt {
        account_id: row.get("account_id"),
        claimed_at: row.get("claimed_at"),
        outcome: row.get("outcome"),
        runner_session_id: row.get("runner_session_id"),
        send_not_after: row.get("send_not_after"),
        send_authorized_at: row.get("send_authorized_at"),
    })
}

async fn check_binding_digest(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    target_id: Uuid,
    attempt_id: Uuid,
    expected: &str,
) -> Result<(), AppError> {
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AppError::invalid_request(
            "invalid pre-registered binding digest",
        ));
    }
    let encrypted: Option<Vec<u8>> = sqlx::query_scalar(
        "SELECT encrypted_binding FROM publication_execution_bindings
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3
           AND target_id=$4 AND attempt_id=$5 FOR SHARE",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project(scope)?)
    .bind(target_id)
    .bind(attempt_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db)?;
    if encrypted.as_deref().map(sha256_hex).as_deref() != Some(expected) {
        return Err(AppError::conflict("publication binding identity differs"));
    }
    Ok(())
}

#[async_trait]
impl PublicationSendAuthorizationRepository for PgPublicationSendAuthorizationRepository {
    async fn register_publication_send(
        &self,
        scope: &TenantScope,
        registration: &RegisterPublicationSend,
    ) -> Result<(), AppError> {
        if registration.runner_session_id.is_nil() || registration.attempt_id.is_nil() {
            return Err(AppError::invalid_request(
                "publication preflight identity required",
            ));
        }
        let mut tx = self.transaction(scope).await?;
        lock_project(&mut tx, scope).await?;
        let target = locked_target(&mut tx, scope, registration.target_id).await?;
        let attempt = locked_attempt(
            &mut tx,
            scope,
            registration.target_id,
            registration.attempt_id,
        )
        .await?;
        if !matches!(
            &target.input,
            ChannelTargetInput::GeneratedPublish {
                rich_payload: Some(_),
                ..
            }
        ) || attempt.account_id != registration.account_id
            || target.input.account_id() != attempt.account_id
            || attempt.outcome.is_some()
        {
            return Err(AppError::conflict(
                "publication preflight does not match the claimed rich attempt",
            ));
        }
        check_binding_digest(
            &mut tx,
            scope,
            registration.target_id,
            registration.attempt_id,
            &registration.encrypted_binding_sha256,
        )
        .await?;
        if let (Some(session), Some(deadline)) = (attempt.runner_session_id, attempt.send_not_after)
        {
            // PostgreSQL timestamptz stores microseconds; compare a repeated
            // chrono request at the precision actually persisted.
            if session != registration.runner_session_id
                || deadline.timestamp_micros() != registration.send_not_after.timestamp_micros()
            {
                return Err(AppError::conflict(
                    "publication preflight is already registered",
                ));
            }
            return Ok(());
        }
        if attempt.runner_session_id.is_some()
            || attempt.send_not_after.is_some()
            || attempt.send_authorized_at.is_some()
        {
            return Err(AppError::conflict("publication preflight is incomplete"));
        }
        let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await
            .map_err(db)?;
        if registration.send_not_after <= now
            || registration.send_not_after > now + MAX_GRANT_LIFETIME
            || registration.send_not_after <= attempt.claimed_at
            || attempt.claimed_at > now
            || now - attempt.claimed_at > MAX_GRANT_LIFETIME
        {
            return Err(AppError::conflict(
                "publication preflight deadline is invalid",
            ));
        }
        sqlx::query(
            "UPDATE channel_execution_attempts SET runner_session_id=$1,send_not_after=$2
             WHERE operator_id=$3 AND tenant_id=$4 AND project_id=$5
               AND target_id=$6 AND attempt_id=$7
               AND outcome IS NULL AND runner_session_id IS NULL
               AND send_not_after IS NULL AND send_authorized_at IS NULL",
        )
        .bind(registration.runner_session_id)
        .bind(registration.send_not_after)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(registration.target_id)
        .bind(registration.attempt_id)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(())
    }

    async fn authorize_publication_send(
        &self,
        scope: &TenantScope,
        expected: &AuthorizePublicationSend,
    ) -> Result<PublicationSendDecision, AppError> {
        let mut tx = self.transaction(scope).await?;
        lock_project(&mut tx, scope).await?;
        let target = locked_target(&mut tx, scope, expected.target_id).await?;
        let attempt =
            locked_attempt(&mut tx, scope, expected.target_id, expected.attempt_id).await?;
        let ChannelTargetInput::GeneratedPublish {
            content_revision_id,
            variant_id,
            publication_intent_id,
            account_id,
            platform,
            title,
            body,
            body_sha256,
            payload_hash,
            distribution_target_id,
            origin_request_id,
            rich_payload: Some(frozen),
            ..
        } = &target.input
        else {
            return Err(AppError::conflict(
                "only a frozen rich publication may be authorized",
            ));
        };
        if attempt.account_id != expected.account_id
            || attempt.account_id != *account_id
            || attempt.outcome.is_some()
            || attempt.runner_session_id != Some(expected.runner_session_id)
            || attempt.send_not_after.is_none()
            || *publication_intent_id != expected.publication_intent_id
            || payload_hash != &expected.payload_hash
        {
            return Err(AppError::conflict(
                "publication attempt or preflight identity differs",
            ));
        }
        check_binding_digest(
            &mut tx,
            scope,
            expected.target_id,
            expected.attempt_id,
            &expected.encrypted_binding_sha256,
        )
        .await?;
        if attempt.send_authorized_at.is_some() {
            return Ok(PublicationSendDecision::AlreadyConsumed);
        }
        // Claim links the very same channel attempt to the original intent.
        let row = sqlx::query(
            "SELECT c.intent_id,c.payload_hash,c.fixture,c.status,c.origin_target_id,c.origin_request_id,
                    a.attempt_id AS distribution_attempt_id
             FROM distribution_publication_commands c
             JOIN distribution_publication_attempts a
               ON a.operator_id=c.operator_id AND a.tenant_id=c.tenant_id
              AND a.project_id=c.project_id AND a.intent_id=c.intent_id
              AND a.command_id=c.command_id
             WHERE c.operator_id=$1 AND c.tenant_id=$2 AND c.project_id=$3
               AND c.command_id=$4 AND c.materialized_target_id=$4
               AND c.intent_id=$5 AND a.attempt_id=$6 FOR UPDATE OF c",
        )
        .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?).bind(expected.target_id)
        .bind(*publication_intent_id).bind(expected.attempt_id)
        .fetch_optional(&mut *tx).await.map_err(db)?
        .ok_or_else(|| AppError::conflict("publication claim has no immutable command"))?;
        if row.get::<bool, _>("fixture")
            || row.get::<String, _>("status") != "claimed"
            || row.get::<String, _>("payload_hash") != *payload_hash
            || row.get::<Option<Uuid>, _>("origin_target_id").is_some()
                == row.get::<Option<Uuid>, _>("origin_request_id").is_some()
            || row
                .get::<Option<Uuid>, _>("origin_target_id")
                .unwrap_or(Uuid::nil())
                != *distribution_target_id
            || row.get::<Option<Uuid>, _>("origin_request_id") != *origin_request_id
        {
            return Err(AppError::conflict("publication command is not executable"));
        }
        let intent_row = sqlx::query(
            "SELECT variant_id,verification,verification_evidence_id,
                    origin_target_id,origin_request_id,body
             FROM distribution_publication_intents
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3
               AND intent_id=$4 FOR UPDATE",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(*publication_intent_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or_else(|| AppError::conflict("publication intent is unavailable"))?;
        let intent: PublicationIntent = decode(intent_row.get("body"))?;
        if intent.intent_id != *publication_intent_id
            || intent.variant_id != *variant_id
            || intent_row.get::<Uuid, _>("variant_id") != *variant_id
            || intent.content_revision_id != *content_revision_id
            || intent.account_id != *account_id
            || intent.platform_id != *platform
            || intent.channel_target_id != *distribution_target_id
            || intent_row
                .get::<Option<Uuid>, _>("origin_target_id")
                .unwrap_or(Uuid::nil())
                != *distribution_target_id
            || intent_row.get::<Option<Uuid>, _>("origin_request_id") != *origin_request_id
            || intent.payload_hash != *payload_hash
            || intent.verification != IntentVerification::Unknown
            || intent.verification_evidence_id != Some(expected.attempt_id)
            || intent_row.get::<String, _>("verification") != "unknown"
            || intent_row.get::<Option<Uuid>, _>("verification_evidence_id")
                != Some(expected.attempt_id)
        {
            return Err(AppError::conflict(
                "publication intent differs from the claimed attempt",
            ));
        }
        let pre_send_unknown: Option<bool> = sqlx::query_scalar(
            "SELECT fixture FROM distribution_intent_evidence
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3
               AND intent_id=$4 AND attempt_id=$5 AND evidence_id=$5
               AND result='unknown' FOR SHARE",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(*publication_intent_id)
        .bind(expected.attempt_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        if pre_send_unknown != Some(false) {
            return Err(AppError::conflict(
                "publication pre-send claim evidence missing",
            ));
        }
        let variant_json: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT body FROM distribution_channel_variants
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3
               AND variant_id=$4 AND content_revision_id=$5 FOR SHARE",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(*variant_id)
        .bind(*content_revision_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        let variant: ChannelVariant = decode(
            variant_json.ok_or_else(|| AppError::conflict("publication variant is unavailable"))?,
        )?;
        validate_rich_publication_payload(&variant)?;
        if variant.variant_id != *variant_id
            || variant.platform_id != *platform
            || variant.title != *title
            || variant.markdown != *body
            || variant.payload_hash != *payload_hash
            || variant.rich_payload.as_ref() != Some(frozen)
            || sha256_hex(body.as_bytes()) != *body_sha256
        {
            return Err(AppError::conflict(
                "publication frozen rich payload differs",
            ));
        }
        let revision_json: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT body FROM content_revisions WHERE operator_id=$1 AND tenant_id=$2
               AND project_id=$3 AND revision_id=$4 FOR SHARE",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(*content_revision_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        let revision: ContentRevision = decode(
            revision_json
                .ok_or_else(|| AppError::conflict("publication revision is unavailable"))?,
        )?;
        if revision.revision_id != *content_revision_id
            || revision.document != frozen.document
            || revision.evidence != variant.evidence
            || revision.markdown != variant.markdown
        {
            return Err(AppError::conflict("publication source revision differs"));
        }
        let keys = frozen
            .media
            .iter()
            .map(|item| item.object.clone())
            .collect::<Vec<_>>();
        let bindings = validate_content_media_in_transaction(&mut tx, scope, &keys).await?;
        for item in &frozen.media {
            if !bindings.iter().any(|binding| {
                binding.binding_id == item.binding_id
                    && binding.image.key == item.object
                    && binding.image.media_type == item.media_type
                    && binding.image.byte_len == item.byte_len
                    && binding.image.width == item.width
                    && binding.image.height == item.height
            }) {
                return Err(AppError::conflict("publication media grant differs"));
            }
        }
        let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await
            .map_err(db)?;
        let deadline = attempt.send_not_after.expect("checked registered deadline");
        if now > deadline || now < attempt.claimed_at {
            return Err(AppError::conflict("publication send authorization expired"));
        }
        let updated = sqlx::query(
            "UPDATE channel_execution_attempts SET send_authorized_at=$1
             WHERE operator_id=$2 AND tenant_id=$3 AND project_id=$4
               AND target_id=$5 AND attempt_id=$6 AND outcome IS NULL
               AND send_authorized_at IS NULL AND runner_session_id=$7
               AND send_not_after >= $1",
        )
        .bind(now)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(expected.target_id)
        .bind(expected.attempt_id)
        .bind(expected.runner_session_id)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        if updated.rows_affected() != 1 {
            return Err(AppError::conflict(
                "publication attempt authorization changed",
            ));
        }
        tx.commit().await.map_err(db)?;
        Ok(PublicationSendDecision::Granted(PublicationSendGrant {
            attempt_id: expected.attempt_id,
            runner_session_id: expected.runner_session_id,
            payload_hash: variant.payload_hash,
            send_not_after: deadline,
        }))
    }
}
