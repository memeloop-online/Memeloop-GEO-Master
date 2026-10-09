use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, ErrorCode, ObservationCapture, ObservationCaptureInput, ObservationCaptureReceipt,
    ObservationProviderIdentity, ProviderCleanupAction, ProviderCleanupBackfillItem,
    ProviderCleanupClaim, ProviderCleanupDiagnostic, ProviderCleanupDueItem,
    ProviderCleanupOutcome, ProviderConversationCleanupRepository, TenantScope,
};
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Clone)]
pub struct PgProviderConversationCleanupRepository {
    pool: PgPool,
}

impl PgProviderConversationCleanupRepository {
    pub fn from_database(database: &crate::Database) -> Self {
        Self {
            pool: database.pool().clone(),
        }
    }
}

fn db(_: sqlx::Error) -> AppError {
    AppError::new(
        ErrorCode::DependencyUnavailable,
        "conversation cleanup store unavailable",
    )
}

fn project(scope: &TenantScope) -> Result<Uuid, AppError> {
    scope
        .project_id
        .map(|id| id.as_uuid())
        .ok_or_else(|| AppError::forbidden("project scope required"))
}

#[async_trait]
impl ProviderConversationCleanupRepository for PgProviderConversationCleanupRepository {
    async fn scan_unqueued(
        &self,
        as_of: DateTime<Utc>,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ProviderCleanupBackfillItem>, AppError> {
        let limit = scan_limit(limit)?;
        let rows = sqlx::query(
            "SELECT o.operator_id,o.tenant_id,o.project_id,o.capture_id \
             FROM observation_captures o WHERE o.stored_at<=$1 \
             AND ($2::uuid IS NULL OR o.capture_id>$2) \
             AND o.input #>> '{original_identity,provider}'=o.input #>> '{owned_conversation,provider}' \
             AND length(o.input #>> '{original_identity,platform_account_id}')>0 \
             AND o.input #>> '{owned_conversation,correlation}' IN ('create_response','verified_page') \
             AND ((o.phase='source' AND o.input #>> '{owned_conversation,purpose}'='measurement') \
               OR (o.phase='extraction' AND o.input #>> '{owned_conversation,purpose}'='extraction')) \
             AND NOT EXISTS (SELECT 1 FROM provider_conversation_cleanup c \
               WHERE c.operator_id=o.operator_id AND c.account_id=o.account_id \
               AND c.provider=o.input #>> '{owned_conversation,provider}' \
               AND c.external_conversation_id=o.input #>> '{owned_conversation,external_conversation_id}') \
             ORDER BY o.capture_id LIMIT $3",
        )
        .bind(as_of).bind(after).bind(limit)
        .fetch_all(&self.pool).await.map_err(db)?;
        Ok(rows
            .into_iter()
            .map(|row| ProviderCleanupBackfillItem {
                scope: row_scope(&row),
                capture_id: row.get("capture_id"),
            })
            .collect())
    }

    async fn scan_due(
        &self,
        as_of: DateTime<Utc>,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ProviderCleanupDueItem>, AppError> {
        let limit = scan_limit(limit)?;
        // Deliberately independent of project pause: cleanup is recovery work.
        // Claim repeats all safety checks against current state.
        let rows = sqlx::query(
            "SELECT operator_id,tenant_id,project_id,cleanup_id \
             FROM provider_conversation_cleanup WHERE state NOT IN ('deleted','archived') \
             AND next_attempt_at<=$1 AND (lease_until IS NULL OR lease_until<=$1) \
             AND ($2::uuid IS NULL OR cleanup_id>$2) ORDER BY cleanup_id LIMIT $3",
        )
        .bind(as_of)
        .bind(after)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        Ok(rows
            .into_iter()
            .map(|row| ProviderCleanupDueItem {
                scope: row_scope(&row),
                cleanup_id: row.get("cleanup_id"),
            })
            .collect())
    }

    async fn enqueue(&self, scope: &TenantScope, capture_id: Uuid) -> Result<Uuid, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        crate::set_local_scope(&mut tx, scope).await.map_err(db)?;
        // Source ownership never stands in for an extraction chat. Only the
        // independently captured browser extraction may register that resource,
        // including raw transport whose JSON interpretation failed.
        let row = sqlx::query(
            "SELECT account_id,input #>> '{owned_conversation,provider}' AS provider, \
             input #>> '{owned_conversation,external_conversation_id}' AS external_id \
             FROM observation_captures WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 \
             AND capture_id=$4 \
             AND input #>> '{original_identity,provider}'=input #>> '{owned_conversation,provider}' \
             AND length(input #>> '{original_identity,platform_account_id}')>0 \
             AND input #>> '{owned_conversation,correlation}' IN ('create_response','verified_page') \
             AND ((phase='source' AND input #>> '{owned_conversation,purpose}'='measurement') \
               OR (phase='extraction' AND input #>> '{owned_conversation,purpose}'='extraction'))",
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
            .bind(project(scope)?).bind(capture_id).fetch_optional(&mut *tx).await.map_err(db)?
            .ok_or_else(|| AppError::conflict("capture has no eligible conversation ownership"))?;
        let account: Uuid = row.get("account_id");
        let provider: String = row.get("provider");
        let external: String = row.get("external_id");
        sqlx::query(
            "INSERT INTO provider_conversation_cleanup \
             (cleanup_id,operator_id,tenant_id,project_id,capture_id,account_id,provider,external_conversation_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT DO NOTHING",
        ).bind(Uuid::new_v4()).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
            .bind(project(scope)?).bind(capture_id).bind(account).bind(&provider).bind(&external)
            .execute(&mut *tx).await.map_err(db)?;
        let id = sqlx::query_scalar(
            "SELECT cleanup_id FROM provider_conversation_cleanup WHERE operator_id=$1 \
             AND tenant_id=$2 AND project_id=$3 AND account_id=$4 AND provider=$5 AND external_conversation_id=$6",
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .bind(account).bind(provider).bind(external).fetch_optional(&mut *tx).await.map_err(db)?
            .ok_or_else(|| AppError::conflict("conversation cleanup belongs to another scope"))?;
        tx.commit().await.map_err(db)?;
        Ok(id)
    }

    async fn claim_due(
        &self,
        scope: &TenantScope,
    ) -> Result<Option<ProviderCleanupClaim>, AppError> {
        self.claim_selected(scope, None).await
    }

    async fn claim(
        &self,
        scope: &TenantScope,
        cleanup_id: Uuid,
    ) -> Result<Option<ProviderCleanupClaim>, AppError> {
        self.claim_selected(scope, Some(cleanup_id)).await
    }

    async fn authorize_delete(
        &self,
        scope: &TenantScope,
        cleanup_id: Uuid,
        lease_id: Uuid,
        reservation_id: Uuid,
    ) -> Result<ProviderCleanupClaim, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        crate::set_local_scope(&mut tx, scope).await.map_err(db)?;
        let account_id: Uuid = sqlx::query_scalar(
            "SELECT account_id FROM provider_conversation_cleanup \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND cleanup_id=$4 \
             AND state='running' AND action='delete' AND lease_id=$5 \
             AND lease_until>clock_timestamp() FOR UPDATE",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(cleanup_id)
        .bind(lease_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or_else(|| AppError::conflict("cleanup deletion lease unavailable"))?;
        // Channel claims take this same row lock before creating an attempt.
        let reserved: Option<Uuid> = sqlx::query_scalar(
            "SELECT reservation_id FROM channel_account_preflight_reservations \
             WHERE operator_id=$1 AND account_id=$2 AND reservation_id=$3 \
             AND expires_at>clock_timestamp() FOR UPDATE",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(account_id)
        .bind(reservation_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        if reserved.is_none() {
            return Err(AppError::conflict(
                "cleanup account reservation unavailable",
            ));
        }
        let row = sqlx::query(
            "SELECT c.*,o.input,o.input_hash,o.stored_at, \
             o.input->'original_identity' AS original_identity \
             FROM provider_conversation_cleanup c JOIN observation_captures o \
             ON o.capture_id=c.capture_id AND o.operator_id=c.operator_id \
             AND o.tenant_id=c.tenant_id AND o.project_id=c.project_id \
             AND o.account_id=c.account_id \
             WHERE c.cleanup_id=$1 AND c.lease_until>clock_timestamp() \
             AND EXISTS (SELECT 1 FROM channel_account_preflight_reservations r \
               WHERE r.operator_id=c.operator_id AND r.account_id=c.account_id \
               AND r.reservation_id=$2 AND r.expires_at>clock_timestamp()) \
             AND NOT EXISTS (SELECT 1 FROM channel_execution_attempts a \
               WHERE a.operator_id=c.operator_id AND a.account_id=c.account_id AND a.received_at IS NULL) \
             AND NOT EXISTS (SELECT 1 FROM observation_captures other \
               WHERE other.operator_id=c.operator_id AND other.account_id=c.account_id \
               AND other.input #>> '{owned_conversation,provider}'=c.provider \
               AND other.input #>> '{owned_conversation,external_conversation_id}'=c.external_conversation_id \
               AND (other.tenant_id<>c.tenant_id OR other.project_id<>c.project_id))",
        ).bind(cleanup_id).bind(reservation_id)
            .fetch_optional(&mut *tx).await.map_err(db)?
            .ok_or_else(|| AppError::conflict("cleanup conversation is not exclusively idle"))?;
        let capture = decode_capture(&row)?;
        let claim = decode_claim(&row, scope)?;
        if claim.retained_message_inventory_sha256.is_none()
            || capture.input.account_id != claim.account_id
            || capture.input.original_identity.as_ref() != Some(&claim.original_identity)
            || !capture
                .input
                .owned_conversation
                .as_ref()
                .is_some_and(|owned| {
                    owned.provider == claim.provider
                        && owned.external_conversation_id == claim.external_conversation_id
                })
        {
            return Err(AppError::conflict(
                "cleanup complete conversation evidence required",
            ));
        }
        tx.commit().await.map_err(db)?;
        Ok(claim)
    }

    async fn finish(
        &self,
        scope: &TenantScope,
        cleanup_id: Uuid,
        lease_id: Uuid,
        outcome: ProviderCleanupOutcome,
    ) -> Result<(), AppError> {
        self.finish_claim(scope, cleanup_id, lease_id, outcome, None)
            .await
    }

    async fn finish_with_diagnostic(
        &self,
        scope: &TenantScope,
        cleanup_id: Uuid,
        lease_id: Uuid,
        outcome: ProviderCleanupOutcome,
        diagnostic: Option<ProviderCleanupDiagnostic>,
    ) -> Result<(), AppError> {
        self.finish_claim(scope, cleanup_id, lease_id, outcome, diagnostic)
            .await
    }
}

fn scan_limit(limit: usize) -> Result<i64, AppError> {
    if !(1..=100).contains(&limit) {
        return Err(AppError::invalid_request(
            "cleanup scan limit must be between 1 and 100",
        ));
    }
    Ok(limit as i64)
}

fn row_scope(row: &sqlx::postgres::PgRow) -> TenantScope {
    TenantScope::new(
        row.get::<Uuid, _>("operator_id").into(),
        row.get::<Uuid, _>("tenant_id").into(),
        Some(row.get::<Uuid, _>("project_id").into()),
    )
}

fn decode_capture(row: &sqlx::postgres::PgRow) -> Result<ObservationCapture, AppError> {
    let input: ObservationCaptureInput = serde_json::from_value(row.get("input"))
        .map_err(|_| AppError::conflict("cleanup retained evidence invalid"))?;
    Ok(ObservationCapture {
        receipt: ObservationCaptureReceipt {
            capture_id: row.get("capture_id"),
            schema_version: 1,
            digest_sha256: row.get("input_hash"),
            stored_at: row.get("stored_at"),
        },
        input,
    })
}

fn decode_claim(
    row: &sqlx::postgres::PgRow,
    scope: &TenantScope,
) -> Result<ProviderCleanupClaim, AppError> {
    let original_identity: ObservationProviderIdentity =
        serde_json::from_value(row.get("original_identity"))
            .map_err(|_| AppError::new(ErrorCode::Internal, "stored cleanup identity invalid"))?;
    original_identity.validate()?;
    Ok(ProviderCleanupClaim {
        cleanup_id: row.get("cleanup_id"),
        capture_id: row.get("capture_id"),
        account_id: row.get("account_id"),
        provider: row.get("provider"),
        external_conversation_id: row.get("external_conversation_id"),
        original_identity,
        retained_message_inventory_sha256: decode_capture(row)?
            .retained_message_inventory_sha256(scope),
        lease_id: row.get("lease_id"),
        lease_until: row.get("lease_until"),
        action: if row.get::<String, _>("action") == "reconcile" {
            ProviderCleanupAction::Reconcile
        } else {
            ProviderCleanupAction::Delete
        },
    })
}

impl PgProviderConversationCleanupRepository {
    async fn claim_selected(
        &self,
        scope: &TenantScope,
        cleanup_id: Option<Uuid>,
    ) -> Result<Option<ProviderCleanupClaim>, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        crate::set_local_scope(&mut tx, scope).await.map_err(db)?;
        // All account attempts, not merely the capture's attempt, must be done.
        // This is a scheduling snapshot, NOT a lock against future channel work.
        let row = sqlx::query(
            "WITH due AS (SELECT c.cleanup_id FROM provider_conversation_cleanup c \
             WHERE c.operator_id=$1 AND c.tenant_id=$2 AND c.project_id=$3 \
             AND ($5::uuid IS NULL OR c.cleanup_id=$5) \
             AND c.state NOT IN ('deleted','archived') \
             AND c.next_attempt_at<=clock_timestamp() \
             AND (c.lease_until IS NULL OR c.lease_until<=clock_timestamp()) \
             AND EXISTS (SELECT 1 FROM observation_captures original \
               WHERE original.capture_id=c.capture_id \
               AND original.operator_id=c.operator_id AND original.tenant_id=c.tenant_id \
               AND original.project_id=c.project_id AND original.account_id=c.account_id \
               AND ((original.phase='source' AND original.input #>> '{owned_conversation,purpose}'='measurement') \
                 OR (original.phase='extraction' AND original.input #>> '{owned_conversation,purpose}'='extraction')) \
               AND original.input #>> '{owned_conversation,provider}'=c.provider \
               AND original.input #>> '{owned_conversation,external_conversation_id}'=c.external_conversation_id \
               AND original.input #>> '{owned_conversation,correlation}' IN ('create_response','verified_page') \
               AND original.input #>> '{original_identity,provider}'=c.provider \
               AND length(original.input #>> '{original_identity,platform_account_id}')>0) \
             AND NOT EXISTS (SELECT 1 FROM channel_execution_attempts a \
               WHERE a.operator_id=c.operator_id AND a.account_id=c.account_id AND a.received_at IS NULL) \
             AND NOT EXISTS (SELECT 1 FROM observation_captures o \
               WHERE o.operator_id=c.operator_id AND o.account_id=c.account_id \
               AND o.input #>> '{owned_conversation,provider}'=c.provider \
               AND o.input #>> '{owned_conversation,external_conversation_id}'=c.external_conversation_id \
               AND (o.tenant_id<>c.tenant_id OR o.project_id<>c.project_id)) \
             ORDER BY c.next_attempt_at,c.cleanup_id LIMIT 1 FOR UPDATE OF c SKIP LOCKED) \
             UPDATE provider_conversation_cleanup c SET \
               action=CASE WHEN c.state IN ('running','unknown','needs_login') THEN 'reconcile' ELSE 'delete' END, \
               state='running',lease_id=$4,lease_until=clock_timestamp()+interval '2 minutes', \
               attempt_count=LEAST(1000,c.attempt_count+1),updated_at=clock_timestamp() \
             FROM due WHERE c.cleanup_id=due.cleanup_id RETURNING c.*, \
               (SELECT original.input->'original_identity' FROM observation_captures original \
                WHERE original.capture_id=c.capture_id) AS original_identity, \
               (SELECT original.input FROM observation_captures original \
                WHERE original.capture_id=c.capture_id) AS input, \
               (SELECT original.input_hash FROM observation_captures original \
                WHERE original.capture_id=c.capture_id) AS input_hash, \
               (SELECT original.stored_at FROM observation_captures original \
                WHERE original.capture_id=c.capture_id) AS stored_at",
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
            .bind(project(scope)?).bind(Uuid::new_v4()).bind(cleanup_id)
            .fetch_optional(&mut *tx).await.map_err(db)?;
        let claim = row
            .as_ref()
            .map(|row| decode_claim(row, scope))
            .transpose()?;
        tx.commit().await.map_err(db)?;
        Ok(claim)
    }

    async fn finish_claim(
        &self,
        scope: &TenantScope,
        cleanup_id: Uuid,
        lease_id: Uuid,
        outcome: ProviderCleanupOutcome,
        diagnostic: Option<ProviderCleanupDiagnostic>,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        crate::set_local_scope(&mut tx, scope).await.map_err(db)?;
        let action: Option<String> = sqlx::query_scalar(
            "SELECT action FROM provider_conversation_cleanup WHERE operator_id=$1 AND tenant_id=$2 \
             AND project_id=$3 AND cleanup_id=$4 AND lease_id=$5 AND state='running' \
             AND lease_until>clock_timestamp() FOR UPDATE",
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
            .bind(project(scope)?).bind(cleanup_id).bind(lease_id)
            .fetch_optional(&mut *tx).await.map_err(db)?;
        let action = match action.as_deref() {
            Some("delete") => ProviderCleanupAction::Delete,
            Some("reconcile") => ProviderCleanupAction::Reconcile,
            _ => return Err(AppError::conflict("cleanup lease expired or replaced")),
        };
        let state = outcome.next_state(action)?;
        let diagnostic = diagnostic
            .map(serde_json::to_value)
            .transpose()
            .map_err(|_| AppError::new(ErrorCode::Internal, "cleanup diagnostic invalid"))?;
        // One immutable summary per fenced finish. Expired/replaced leases
        // cannot append a receipt or overwrite the latest diagnostic.
        sqlx::query(
            "INSERT INTO provider_conversation_cleanup_attempts \
             (cleanup_id,lease_id,operator_id,tenant_id,project_id,action,state,diagnostic) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
        )
        .bind(cleanup_id)
        .bind(lease_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(match action {
            ProviderCleanupAction::Delete => "delete",
            ProviderCleanupAction::Reconcile => "reconcile",
        })
        .bind(state)
        .bind(&diagnostic)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        sqlx::query(
            "UPDATE provider_conversation_cleanup SET state=$1,lease_id=NULL,lease_until=NULL, \
             next_attempt_at=clock_timestamp()+make_interval(secs => LEAST(3600,30*attempt_count)), \
             updated_at=clock_timestamp(),last_diagnostic=$3 WHERE cleanup_id=$2",
        ).bind(state).bind(cleanup_id).bind(diagnostic).execute(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(())
    }
}
