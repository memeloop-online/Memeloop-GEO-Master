use async_trait::async_trait;
use geo_domain::{
    AppError, ErrorCode, ObservationProviderIdentity, ProviderCleanupAction, ProviderCleanupClaim,
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
        let mut tx = self.pool.begin().await.map_err(db)?;
        crate::set_local_scope(&mut tx, scope).await.map_err(db)?;
        // All account attempts, not merely the capture's attempt, must be done.
        // This is a scheduling snapshot, NOT a lock against future channel work.
        let row = sqlx::query(
            "WITH due AS (SELECT c.cleanup_id FROM provider_conversation_cleanup c \
             WHERE c.operator_id=$1 AND c.tenant_id=$2 AND c.project_id=$3 \
             AND c.state NOT IN ('deleted','archived') \
             AND c.next_attempt_at<=clock_timestamp() \
             AND (c.lease_until IS NULL OR c.lease_until<=clock_timestamp()) \
             AND EXISTS (SELECT 1 FROM observation_captures original \
               WHERE original.capture_id=c.capture_id \
               AND original.phase IN ('source','extraction') \
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
                WHERE original.capture_id=c.capture_id) AS original_identity",
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
            .bind(project(scope)?).bind(Uuid::new_v4()).fetch_optional(&mut *tx).await.map_err(db)?;
        let claim = row
            .map(|row| {
                let original_identity: ObservationProviderIdentity =
                    serde_json::from_value(row.get("original_identity")).map_err(|_| {
                        AppError::new(ErrorCode::Internal, "stored cleanup identity invalid")
                    })?;
                original_identity.validate()?;
                Ok::<_, AppError>(ProviderCleanupClaim {
                    cleanup_id: row.get("cleanup_id"),
                    capture_id: row.get("capture_id"),
                    account_id: row.get("account_id"),
                    provider: row.get("provider"),
                    external_conversation_id: row.get("external_conversation_id"),
                    original_identity,
                    lease_id: row.get("lease_id"),
                    lease_until: row.get("lease_until"),
                    action: if row.get::<String, _>("action") == "reconcile" {
                        ProviderCleanupAction::Reconcile
                    } else {
                        ProviderCleanupAction::Delete
                    },
                })
            })
            .transpose()?;
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
        sqlx::query(
            "UPDATE provider_conversation_cleanup SET state=$1,lease_id=NULL,lease_until=NULL, \
             next_attempt_at=clock_timestamp()+make_interval(secs => LEAST(3600,30*attempt_count)), \
             updated_at=clock_timestamp() WHERE cleanup_id=$2",
        ).bind(state).bind(cleanup_id).execute(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(())
    }
}
