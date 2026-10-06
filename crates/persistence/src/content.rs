use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, ContentAsset, ContentBrief, ContentCheck, ContentExecution, ContentFinding,
    ContentHandoff, ContentItem, ContentItemStatus, ContentRepository, ContentReuseBinding,
    ContentReuseDecision, ContentReuseRequest, ContentRevision, ContentSemanticDescriptor,
    ContentState, ContentStep, DocumentManifest, ErrorCode, StepLease, StructuredDocument,
    TenantScope, start_content_state,
};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

// Only work that can still create a coverage row or a logical publication
// intent is a closed-stage dispatch candidate. Unknown/sent intents and
// permanently unsupported cells must not cause hot-loop rescans.
const CLOSED_WORK_ELIGIBLE: &str = "\
    (NOT EXISTS (SELECT 1 FROM distribution_execution_manifests d WHERE \
      (d.operator_id,d.tenant_id,d.project_id,d.cycle_id)= \
      (c.operator_id,c.tenant_id,c.project_id,c.cycle_id)) \
     OR EXISTS (SELECT 1 FROM distribution_execution_manifests d WHERE \
       (d.operator_id,d.tenant_id,d.project_id,d.cycle_id)= \
       (c.operator_id,c.tenant_id,c.project_id,c.cycle_id) \
       AND d.content_execution_id=c.execution_id \
       AND (NOT d.complete OR EXISTS (SELECT 1 FROM distribution_execution_targets t \
         WHERE (t.operator_id,t.tenant_id,t.project_id,t.manifest_id)= \
           (d.operator_id,d.tenant_id,d.project_id,d.manifest_id) \
         AND (t.current_body->>'status'='pending' \
              OR (t.current_body->>'status'='deferred' AND t.current_body->>'reason' IN \
                ('account_unassigned','source_unavailable','source_changed','content_unsupported')))))))";

#[derive(Debug, Clone)]
pub struct ContentDispatchCandidate {
    pub scope: TenantScope,
    pub execution_id: Uuid,
}

#[derive(Debug, Clone)]
pub struct ContentDispatchLease {
    pub scope: TenantScope,
    pub execution_id: Uuid,
    pub token: Uuid,
}

#[derive(Clone)]
pub struct PgContentRepository {
    pool: PgPool,
}
impl PgContentRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub fn from_database(database: &crate::Database) -> Self {
        Self::new(database.pool().clone())
    }
    /// Keyset enumeration is independent of dispatch claims so one failed
    /// execution cannot starve later executions in the same scan.
    pub async fn scan_running_after(
        &self,
        after: Option<Uuid>,
        now: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<ContentDispatchCandidate>, AppError> {
        if limit == 0 || limit > 1000 {
            return Err(AppError::invalid_request("invalid content scan page size"));
        }
        let rows = sqlx::query(
            "SELECT c.operator_id,c.tenant_id,c.project_id,c.execution_id \
             FROM content_executions c JOIN projects p \
               ON p.operator_id=c.operator_id AND p.tenant_id=c.tenant_id AND p.project_id=c.project_id \
             WHERE ($1::uuid IS NULL OR c.execution_id>$1) \
               AND c.state->'execution'->>'status'='running' AND p.status='active' \
               AND (c.dispatch_expires_at IS NULL OR c.dispatch_expires_at<=$2) \
               AND (c.dispatch_retry_after IS NULL OR c.dispatch_retry_after<=$2) \
             ORDER BY c.execution_id LIMIT $3",
        )
        .bind(after)
        .bind(now)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        Ok(rows
            .into_iter()
            .map(|row| ContentDispatchCandidate {
                scope: TenantScope::new(
                    row.get::<Uuid, _>("operator_id").into(),
                    row.get::<Uuid, _>("tenant_id").into(),
                    Some(row.get::<Uuid, _>("project_id").into()),
                ),
                execution_id: row.get("execution_id"),
            })
            .collect())
    }
    /// Independent closed-stage scan works even without a configured model.
    /// A later page can never be hidden by an earlier deferred target.
    pub async fn scan_closed_after(
        &self,
        after: Option<Uuid>,
        now: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<ContentDispatchCandidate>, AppError> {
        if !(1..=1000).contains(&limit) {
            return Err(AppError::invalid_request("invalid content scan page size"));
        }
        let query = format!(
            "SELECT c.operator_id,c.tenant_id,c.project_id,c.execution_id \
             FROM content_executions c JOIN projects p \
               ON (p.operator_id,p.tenant_id,p.project_id)=(c.operator_id,c.tenant_id,c.project_id) \
             WHERE ($1::uuid IS NULL OR c.execution_id>$1) \
               AND c.state->'execution'->>'status'='closed' AND p.status='active' \
               AND (c.dispatch_expires_at IS NULL OR c.dispatch_expires_at<=$2) \
               AND (c.dispatch_retry_after IS NULL OR c.dispatch_retry_after<=$2) \
               AND {CLOSED_WORK_ELIGIBLE} ORDER BY c.execution_id LIMIT $3"
        );
        let rows = sqlx::query(&query)
            .bind(after)
            .bind(now)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(db)?;
        Ok(rows
            .into_iter()
            .map(|row| ContentDispatchCandidate {
                scope: TenantScope::new(
                    row.get::<Uuid, _>("operator_id").into(),
                    row.get::<Uuid, _>("tenant_id").into(),
                    Some(row.get::<Uuid, _>("project_id").into()),
                ),
                execution_id: row.get("execution_id"),
            })
            .collect())
    }
    /// Serializes with content step transitions and status changes on the
    /// execution row. A live old step lease delays takeover until it expires.
    pub async fn try_claim_dispatch(
        &self,
        scope: &TenantScope,
        id: Uuid,
        now: DateTime<Utc>,
        ttl: chrono::Duration,
    ) -> Result<Option<ContentDispatchLease>, AppError> {
        if ttl <= chrono::Duration::zero() {
            return Err(AppError::invalid_request(
                "dispatch lease lifetime must be positive",
            ));
        }
        let Some(project) = scope.project_id else {
            return Err(AppError::invalid_request("project scope required"));
        };
        let mut tx = self.transaction(scope).await?;
        let row = sqlx::query(
            "SELECT c.state,c.dispatch_expires_at,c.dispatch_retry_after FROM content_executions c \
             JOIN projects p ON p.operator_id=c.operator_id AND p.tenant_id=c.tenant_id AND p.project_id=c.project_id \
             WHERE c.operator_id=$1 AND c.tenant_id=$2 AND c.project_id=$3 AND c.execution_id=$4 \
               AND p.status='active' FOR UPDATE OF c",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project.as_uuid())
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        let Some(row) = row else { return Ok(None) };
        let state = decode(row.get("state"))?;
        if state.execution.status == geo_domain::ContentExecutionStatus::Cancelled
            || row
                .get::<Option<DateTime<Utc>>, _>("dispatch_expires_at")
                .is_some_and(|t| t > now)
            || row
                .get::<Option<DateTime<Utc>>, _>("dispatch_retry_after")
                .is_some_and(|t| t > now)
        {
            return Ok(None);
        }
        if state.execution.status == geo_domain::ContentExecutionStatus::Closed {
            let query = format!(
                "SELECT {CLOSED_WORK_ELIGIBLE} AS eligible FROM content_executions c \
                 WHERE c.operator_id=$1 AND c.tenant_id=$2 AND c.project_id=$3 AND c.execution_id=$4"
            );
            let eligible: bool = sqlx::query_scalar(&query)
                .bind(scope.operator_id.as_uuid())
                .bind(scope.tenant_id.as_uuid())
                .bind(project.as_uuid())
                .bind(id)
                .fetch_one(&mut *tx)
                .await
                .map_err(db)?;
            if !eligible {
                return Ok(None);
            }
        }
        if let Some(until) = state
            .items
            .iter()
            .flat_map(|item| &item.steps)
            .filter(|step| step.expires_at > now)
            .map(|step| step.expires_at)
            .max()
        {
            sqlx::query(
                "UPDATE content_executions SET dispatch_retry_after=$1 WHERE execution_id=$2",
            )
            .bind(until)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
            tx.commit().await.map_err(db)?;
            return Ok(None);
        }
        let token = Uuid::new_v4();
        sqlx::query(
            "UPDATE content_executions SET dispatch_token=$1,dispatch_expires_at=$2,dispatch_retry_after=NULL WHERE execution_id=$3",
        )
        .bind(token)
        .bind(now + ttl)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(Some(ContentDispatchLease {
            scope: scope.clone(),
            execution_id: id,
            token,
        }))
    }
    pub async fn renew_dispatch(
        &self,
        lease: &ContentDispatchLease,
        now: DateTime<Utc>,
        ttl: chrono::Duration,
    ) -> Result<bool, AppError> {
        if ttl <= chrono::Duration::zero() {
            return Err(AppError::invalid_request(
                "dispatch lease lifetime must be positive",
            ));
        }
        let updated = sqlx::query(
            "UPDATE content_executions SET dispatch_expires_at=$1 \
             WHERE operator_id=$2 AND tenant_id=$3 AND project_id=$4 AND execution_id=$5 \
               AND dispatch_token=$6 AND dispatch_expires_at>$7 \
               AND state->'execution'->>'status' IN ('running','closed') \
               AND EXISTS (SELECT 1 FROM projects p WHERE p.operator_id=$2 AND p.tenant_id=$3 \
                   AND p.project_id=$4 AND p.status='active')",
        )
        .bind(now + ttl)
        .bind(lease.scope.operator_id.as_uuid())
        .bind(lease.scope.tenant_id.as_uuid())
        .bind(
            lease
                .scope
                .project_id
                .ok_or_else(|| AppError::invalid_request("project scope required"))?
                .as_uuid(),
        )
        .bind(lease.execution_id)
        .bind(lease.token)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(updated.rows_affected() == 1)
    }
    pub async fn release_dispatch(
        &self,
        lease: &ContentDispatchLease,
        now: DateTime<Utc>,
        backoff: chrono::Duration,
    ) -> Result<bool, AppError> {
        let updated = sqlx::query(
            "UPDATE content_executions SET dispatch_token=NULL, dispatch_expires_at=NULL, \
             dispatch_retry_after=$1 WHERE operator_id=$2 AND tenant_id=$3 AND project_id=$4 \
             AND execution_id=$5 AND dispatch_token=$6",
        )
        .bind(now + backoff)
        .bind(lease.scope.operator_id.as_uuid())
        .bind(lease.scope.tenant_id.as_uuid())
        .bind(
            lease
                .scope
                .project_id
                .ok_or_else(|| AppError::invalid_request("project scope required"))?
                .as_uuid(),
        )
        .bind(lease.execution_id)
        .bind(lease.token)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(updated.rows_affected() == 1)
    }
    async fn transaction<'a>(
        &'a self,
        scope: &TenantScope,
    ) -> Result<Transaction<'a, Postgres>, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        crate::set_local_scope(&mut tx, scope).await.map_err(db)?;
        Ok(tx)
    }
    // Project -> execution -> registry is the lock order for every content
    // transition. In particular source changes in another transaction cannot
    // race a reuse decision: source rows are locked FOR SHARE below.
    async fn lock_project(
        tx: &mut Transaction<'_, Postgres>,
        scope: &TenantScope,
    ) -> Result<(), AppError> {
        let project = scope
            .project_id
            .ok_or_else(|| AppError::invalid_request("project scope required"))?;
        let status: Option<String> = sqlx::query_scalar(
            "SELECT status FROM projects WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 FOR UPDATE",
        )
        .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
        .fetch_optional(&mut **tx).await.map_err(db)?;
        if status.is_none() {
            return Err(AppError::not_found("content project not found"));
        }
        Ok(())
    }
    async fn validate_reuse_sources(
        tx: &mut Transaction<'_, Postgres>,
        scope: &TenantScope,
        execution_id: Uuid,
        descriptor: &ContentSemanticDescriptor,
    ) -> Result<(), AppError> {
        Self::validate_frozen_sources(
            tx,
            scope,
            execution_id,
            &descriptor.document_key,
            &descriptor.source_version_ids,
            &descriptor.evidence,
        )
        .await
    }
    async fn validate_frozen_sources(
        tx: &mut Transaction<'_, Postgres>,
        scope: &TenantScope,
        execution_id: Uuid,
        document_key: &str,
        source_version_ids: &[Uuid],
        evidence: &[geo_domain::ContentEvidence],
    ) -> Result<(), AppError> {
        let project = scope
            .project_id
            .ok_or_else(|| AppError::invalid_request("project scope required"))?;
        let active: bool = sqlx::query_scalar(
            "SELECT status='active' FROM projects WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3",
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
            .fetch_one(&mut **tx).await.map_err(db)?;
        if !active {
            return Err(AppError::conflict("content requires an active project"));
        }
        let release: Option<Uuid> = sqlx::query_scalar(
            "SELECT mi.knowledge_release_id FROM content_executions c \
             JOIN document_manifest_items mi ON (mi.operator_id,mi.tenant_id,mi.project_id,mi.manifest_id)= \
                 (c.operator_id,c.tenant_id,c.project_id,c.manifest_id) \
             WHERE c.operator_id=$1 AND c.tenant_id=$2 AND c.project_id=$3 \
             AND c.execution_id=$4 AND mi.document_key=$5",
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
            .bind(execution_id).bind(document_key).fetch_optional(&mut **tx).await.map_err(db)?;
        let release =
            release.ok_or_else(|| AppError::conflict("frozen document release is missing"))?;
        for source_version_id in source_version_ids {
            let eligible: Option<bool> = sqlx::query_scalar(
                "SELECT s.state='active' AND s.purpose='public' AND s.current_version_id=v.source_version_id \
                 FROM knowledge_source_versions v JOIN knowledge_sources s \
                   ON (s.operator_id,s.tenant_id,s.project_id,s.source_id)= \
                      (v.operator_id,v.tenant_id,v.project_id,v.source_id) \
                 JOIN knowledge_release_source_versions r \
                   ON (r.operator_id,r.tenant_id,r.project_id,r.source_version_id)= \
                      (v.operator_id,v.tenant_id,v.project_id,v.source_version_id) \
                 WHERE v.operator_id=$1 AND v.tenant_id=$2 AND v.project_id=$3 \
                   AND v.source_version_id=$4 AND r.knowledge_release_id=$5 FOR SHARE OF s",
            ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
                .bind(*source_version_id).bind(release).fetch_optional(&mut **tx).await.map_err(db)?;
            if eligible != Some(true) {
                return Err(AppError::conflict("frozen source is not currently public"));
            }
        }
        for evidence in evidence {
            let chunk = evidence
                .reference
                .chunk_id
                .ok_or_else(|| AppError::invalid_request("located evidence required"))?;
            let row = sqlx::query(
                "SELECT text,locator FROM knowledge_chunks WHERE operator_id=$1 AND tenant_id=$2 \
                 AND project_id=$3 AND source_version_id=$4 AND chunk_id=$5",
            )
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project.as_uuid())
            .bind(evidence.reference.source_version_id)
            .bind(chunk)
            .fetch_optional(&mut **tx)
            .await
            .map_err(db)?;
            let Some(row) = row else {
                return Err(AppError::conflict("frozen evidence is unavailable"));
            };
            let locator = serde_json::to_value(&evidence.reference.locator).map_err(encode)?;
            let text: String = row.get("text");
            let expected_quote: String = if matches!(
                evidence.reference.locator,
                geo_domain::ChunkLocator::Csv { .. }
            ) {
                text
            } else {
                text.chars().take(1600).collect()
            };
            if row.get::<serde_json::Value, _>("locator") != locator
                || expected_quote != evidence.exact_quote
                || evidence.exact_quote.chars().count() > 1600
            {
                return Err(AppError::conflict("frozen evidence quote changed"));
            }
        }
        Ok(())
    }
    async fn read(&self, scope: &TenantScope, id: Uuid) -> Result<Option<ContentState>, AppError> {
        let Some(project) = scope.project_id else {
            return Err(AppError::invalid_request("project scope required"));
        };
        let mut tx = self.transaction(scope).await?;
        let json: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT state FROM content_executions WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND execution_id=$4",
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
            .bind(id).fetch_optional(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        json.map(decode).transpose()
    }
    async fn mutate<T>(
        &self,
        scope: &TenantScope,
        id: Uuid,
        change: impl FnOnce(&mut ContentState) -> Result<T, AppError> + Send,
    ) -> Result<T, AppError>
    where
        T: Send,
    {
        let Some(project) = scope.project_id else {
            return Err(AppError::invalid_request("project scope required"));
        };
        let mut tx = self.transaction(scope).await?;
        Self::lock_project(&mut tx, scope).await?;
        let json: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT state FROM content_executions WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND execution_id=$4 FOR UPDATE",
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
            .bind(id).fetch_optional(&mut *tx).await.map_err(db)?;
        let mut state =
            decode(json.ok_or_else(|| AppError::not_found("content execution not found"))?)?;
        let was_running = state.execution.status == geo_domain::ContentExecutionStatus::Running;
        let previous_briefs: Vec<Uuid> = state
            .items
            .iter()
            .filter(|i| i.brief.is_some())
            .map(|i| i.item_id)
            .collect();
        let previous_revision_count = state.revisions.len();
        let previous_check_count = state.checks.len();
        let previous_handoff_count = state.handoffs.len();
        let previous_reservations: Vec<_> = state
            .items
            .iter()
            .filter_map(|i| {
                Some((
                    i.item_id,
                    i.semantic_fingerprint.clone()?,
                    i.reuse_reservation_token?,
                ))
            })
            .collect();
        let result = change(&mut state)?;
        for (item_id, fingerprint, token) in previous_reservations {
            if state.items.iter().any(|i| {
                i.item_id == item_id
                    && (i.reuse_reservation_token != Some(token)
                        || matches!(
                            i.status,
                            ContentItemStatus::Blocked | ContentItemStatus::Cancelled
                        ))
            }) {
                sqlx::query("UPDATE content_reuse_registry SET reservation_execution_id=NULL, \
                     reservation_item_id=NULL,reservation_token=NULL,reservation_expires_at=NULL,updated_at=now() \
                     WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND fingerprint=$4 \
                     AND reservation_execution_id=$5 AND reservation_item_id=$6 AND reservation_token=$7")
                    .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
                    .bind(project.as_uuid()).bind(fingerprint).bind(id).bind(item_id).bind(token)
                    .execute(&mut *tx).await.map_err(db)?;
            }
        }
        // Manual edits and copy-on-write forks preserve semantic inputs but
        // explicitly discard the previous producer fence. A fresh independent
        // Check claim acquires the shared fingerprint reservation. A competing
        // producer with a live lease remains authoritative until it finishes
        // or expires, while the edited branch stays drafted and retryable.
        for item in state.items.iter_mut().filter(|i| {
            i.status == ContentItemStatus::Drafted
                && i.semantic_descriptor.is_some()
                && i.reuse_reservation_token.is_none()
        }) {
            let Some(check_lease) = item
                .steps
                .iter()
                .find(|step| step.step == ContentStep::Check && step.expires_at > Utc::now())
            else {
                continue;
            };
            let fingerprint = item
                .semantic_fingerprint
                .as_ref()
                .ok_or_else(|| AppError::conflict("semantic fingerprint missing"))?;
            let row = sqlx::query(
                "SELECT descriptor,reservation_token,reservation_expires_at \
                 FROM content_reuse_registry WHERE operator_id=$1 AND tenant_id=$2 \
                 AND project_id=$3 AND fingerprint=$4 FOR UPDATE",
            )
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project.as_uuid())
            .bind(fingerprint)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db)?
            .ok_or_else(|| AppError::conflict("semantic registry missing"))?;
            if row.get::<serde_json::Value, _>("descriptor")
                != serde_json::to_value(item.semantic_descriptor.as_ref().expect("filtered"))
                    .map_err(encode)?
            {
                return Err(AppError::conflict(
                    "semantic fingerprint descriptor collision",
                ));
            }
            if row.get::<Option<Uuid>, _>("reservation_token").is_some()
                && row
                    .get::<Option<DateTime<Utc>>, _>("reservation_expires_at")
                    .is_some_and(|expires| expires > Utc::now())
            {
                return Err(AppError::conflict("semantic producer is still active"));
            }
            sqlx::query("UPDATE content_reuse_registry SET reservation_execution_id=$1,reservation_item_id=$2, \
                 reservation_token=$3,reservation_expires_at=$4,updated_at=now() \
                 WHERE operator_id=$5 AND tenant_id=$6 AND project_id=$7 AND fingerprint=$8")
                .bind(id).bind(item.item_id).bind(check_lease.token).bind(check_lease.expires_at)
                .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
                .bind(fingerprint).execute(&mut *tx).await.map_err(db)?;
            item.reuse_reservation_token = Some(check_lease.token);
        }
        for item in state.items.iter().filter(|i| {
            i.status == ContentItemStatus::Drafted
                && i.semantic_fingerprint.is_some()
                && i.steps
                    .iter()
                    .any(|step| step.step == ContentStep::Check && step.expires_at > Utc::now())
        }) {
            let owned: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM content_reuse_registry WHERE operator_id=$1 \
                 AND tenant_id=$2 AND project_id=$3 AND fingerprint=$4 \
                 AND reservation_execution_id=$5 AND reservation_item_id=$6 AND reservation_token=$7)",
            ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
                .bind(project.as_uuid()).bind(item.semantic_fingerprint.as_ref().expect("filtered"))
                .bind(id).bind(item.item_id).bind(item.reuse_reservation_token)
                .fetch_one(&mut *tx).await.map_err(db)?;
            if !owned {
                return Err(AppError::conflict(
                    "semantic producer reservation was superseded",
                ));
            }
        }
        if state.handoffs.len() > previous_handoff_count {
            for item in state
                .items
                .iter()
                .filter(|i| i.status == ContentItemStatus::Ready)
            {
                let quotes = item
                    .brief
                    .as_ref()
                    .map(|b| b.quotes.as_slice())
                    .unwrap_or(&[]);
                Self::validate_frozen_sources(
                    &mut tx,
                    scope,
                    id,
                    &item.document_key,
                    &item.source_version_refs,
                    quotes,
                )
                .await?;
            }
        }
        // The first producer's reservation is held through the successful
        // independent check. A late model result can never become ready after
        // another execution has taken over the fingerprint.
        for item in state.items.iter().filter(|item| {
            item.status == ContentItemStatus::Ready
                && item.semantic_descriptor.is_some()
                && item.reuse_binding.is_none()
                && state.checks[previous_check_count..]
                    .iter()
                    .any(|check| Some(check.revision_id) == item.ready_revision_id)
        }) {
            Self::validate_reuse_sources(
                &mut tx,
                scope,
                id,
                item.semantic_descriptor.as_ref().expect("filtered"),
            )
            .await?;
            let fingerprint = item
                .semantic_fingerprint
                .as_ref()
                .ok_or_else(|| AppError::conflict("semantic fingerprint missing"))?;
            let token = item
                .reuse_reservation_token
                .ok_or_else(|| AppError::conflict("producer reservation missing"))?;
            let owned: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM content_reuse_registry WHERE operator_id=$1 AND tenant_id=$2 \
                 AND project_id=$3 AND fingerprint=$4 AND reservation_execution_id=$5 \
                 AND reservation_item_id=$6 AND reservation_token=$7 AND reservation_expires_at>now())",
            ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
                .bind(fingerprint).bind(id).bind(item.item_id).bind(token)
                .fetch_one(&mut *tx).await.map_err(db)?;
            if !owned {
                return Err(AppError::conflict(
                    "content producer reservation was superseded",
                ));
            }
        }
        for item in state
            .items
            .iter()
            .filter(|i| i.brief.is_some() && !previous_briefs.contains(&i.item_id))
        {
            let brief = item.brief.as_ref().expect("filtered");
            sqlx::query("INSERT INTO content_briefs (brief_id,operator_id,tenant_id,project_id,execution_id,item_id,body,created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)")
                .bind(brief.brief_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
                .bind(id).bind(item.item_id).bind(serde_json::to_value(brief).map_err(encode)?).bind(brief.created_at)
                .execute(&mut *tx).await.map_err(db)?;
        }
        // Append immutable revision/hand-off records in the same transaction as
        // the updated execution state. A stale lease or cancelled execution
        // returns above without writing anything.
        for revision in &state.revisions[previous_revision_count..] {
            sqlx::query("INSERT INTO content_revisions (revision_id,operator_id,tenant_id,project_id,execution_id,asset_id,revision,body,created_at,derived_from_revision_id) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
                .bind(revision.revision_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
                .bind(id).bind(revision.asset_id).bind(revision.revision)
                .bind(serde_json::to_value(revision).map_err(encode)?).bind(revision.created_at)
                .bind(revision.derived_from_revision_id)
                .execute(&mut *tx).await.map_err(db)?;
        }
        for check in &state.checks[previous_check_count..] {
            sqlx::query("INSERT INTO content_checks (check_id,operator_id,tenant_id,project_id,execution_id,revision_id,body,created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)")
                .bind(check.check_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
                .bind(id).bind(check.revision_id).bind(serde_json::to_value(check).map_err(encode)?).bind(check.created_at)
                .execute(&mut *tx).await.map_err(db)?;
        }
        // Older executions can finish after the one-time migration backfill.
        // Index those proof-incomplete ready branches in this same check
        // transaction so a successor cannot silently generate a new identity.
        for item in state.items.iter().filter(|item| {
            item.status == ContentItemStatus::Ready
                && item.semantic_descriptor.is_none()
                && state.checks[previous_check_count..].iter().any(|check| {
                    Some(check.revision_id) == item.ready_revision_id
                        && !check.findings.iter().any(|finding| finding.blocking)
                })
        }) {
            sqlx::query("INSERT INTO content_reuse_legacy_branches \
                 (operator_id,tenant_id,project_id,execution_id,item_id,document_key,source_version_refs) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING")
                .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
                .bind(project.as_uuid()).bind(id).bind(item.item_id).bind(&item.document_key)
                .bind(serde_json::to_value(&item.source_version_refs).map_err(encode)?)
                .execute(&mut *tx).await.map_err(db)?;
        }
        for item in &state.items {
            let Some(fingerprint) = &item.semantic_fingerprint else {
                continue;
            };
            if let Some(token) = item.reuse_reservation_token {
                let expires = item.steps.iter().map(|lease| lease.expires_at).max();
                if let Some(expires) = expires {
                    sqlx::query("UPDATE content_reuse_registry SET reservation_expires_at=GREATEST(reservation_expires_at,$1),updated_at=now() \
                         WHERE operator_id=$2 AND tenant_id=$3 AND project_id=$4 AND fingerprint=$5 \
                         AND reservation_execution_id=$6 AND reservation_item_id=$7 AND reservation_token=$8")
                        .bind(expires).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
                        .bind(project.as_uuid()).bind(fingerprint).bind(id).bind(item.item_id).bind(token)
                        .execute(&mut *tx).await.map_err(db)?;
                }
            }
            if item.status == ContentItemStatus::Ready
                && item.reuse_binding.is_none()
                && let Some(revision_id) = item.ready_revision_id
                && let Some(check) = state.checks[previous_check_count..].iter().find(|c| {
                    c.revision_id == revision_id && !c.findings.iter().any(|f| f.blocking)
                })
            {
                sqlx::query("UPDATE content_reuse_registry SET origin_execution_id=$1,origin_item_id=$2,asset_id=$3,revision_id=$4,check_id=$5, \
                    reservation_execution_id=NULL,reservation_item_id=NULL,reservation_token=NULL,reservation_expires_at=NULL,updated_at=now() \
                    WHERE operator_id=$6 AND tenant_id=$7 AND project_id=$8 AND fingerprint=$9")
                    .bind(id).bind(item.item_id).bind(item.asset_id.ok_or_else(|| AppError::conflict("asset missing"))?)
                    .bind(revision_id).bind(check.check_id).bind(scope.operator_id.as_uuid())
                    .bind(scope.tenant_id.as_uuid()).bind(project.as_uuid()).bind(fingerprint)
                    .execute(&mut *tx).await.map_err(db)?;
            }
        }
        // An edit, recheck, or invalidation may supersede a candidate. Keep
        // the immutable old check, but cease offering its outdated revision.
        let active_candidates: Vec<Uuid> = state
            .items
            .iter()
            .filter(|i| i.status == ContentItemStatus::Ready && i.reuse_binding.is_none())
            .filter_map(|i| i.ready_revision_id)
            .collect();
        sqlx::query("UPDATE content_reuse_registry SET origin_execution_id=NULL,origin_item_id=NULL,asset_id=NULL,revision_id=NULL,check_id=NULL,updated_at=now() \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND origin_execution_id=$4 \
             AND NOT (revision_id=ANY($5))")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
            .bind(id).bind(&active_candidates).execute(&mut *tx).await.map_err(db)?;
        for handoff in &state.handoffs[previous_handoff_count..] {
            sqlx::query("INSERT INTO content_handoffs (handoff_id,operator_id,tenant_id,project_id,execution_id,revision,supersedes_handoff_id,body,created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)")
                    .bind(handoff.handoff_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
                    .bind(id).bind(handoff.revision).bind(handoff.supersedes_handoff_id)
                    .bind(serde_json::to_value(handoff).map_err(encode)?).bind(handoff.created_at)
                    .execute(&mut *tx).await.map_err(db)?;
        }
        // The previous workflow may have deferred dispatch until a now-finished
        // item step lease expires. Once a closed handoff is persisted, that
        // step-based delay must not hide the new distribution stage. Later
        // closed-stage retry backoff remains intact on idempotent close calls.
        let just_closed =
            was_running && state.execution.status == geo_domain::ContentExecutionStatus::Closed;
        sqlx::query(
            "UPDATE content_executions SET state=$1, \
             dispatch_retry_after=CASE WHEN $6 THEN NULL ELSE dispatch_retry_after END \
             WHERE operator_id=$2 AND tenant_id=$3 AND project_id=$4 AND execution_id=$5",
        )
        .bind(serde_json::to_value(state).map_err(encode)?)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project.as_uuid())
        .bind(id)
        .bind(just_closed)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(result)
    }
    async fn asset_execution(&self, scope: &TenantScope, asset_id: Uuid) -> Result<Uuid, AppError> {
        let Some(project) = scope.project_id else {
            return Err(AppError::invalid_request("project scope required"));
        };
        let mut tx = self.transaction(scope).await?;
        let id: Option<Uuid> = sqlx::query_scalar(
            "SELECT execution_id FROM content_revisions WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND asset_id=$4 LIMIT 1",
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
            .bind(asset_id).fetch_optional(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        id.ok_or_else(|| AppError::not_found("content asset not found"))
    }
}
fn db(error: sqlx::Error) -> AppError {
    AppError::new(
        ErrorCode::DependencyUnavailable,
        format!("content database operation failed: {error}"),
    )
}
fn encode(_: serde_json::Error) -> AppError {
    AppError::new(ErrorCode::Internal, "cannot encode content state")
}
fn decode(json: serde_json::Value) -> Result<ContentState, AppError> {
    serde_json::from_value(json)
        .map_err(|_| AppError::new(ErrorCode::Internal, "stored content state invalid"))
}
#[async_trait]
impl ContentRepository for PgContentRepository {
    async fn prepare_or_reuse(
        &self,
        scope: &TenantScope,
        request: ContentReuseRequest,
    ) -> Result<ContentReuseDecision, AppError> {
        let descriptor = request.descriptor.canonical()?;
        if &descriptor.scope != scope {
            return Err(AppError::conflict("semantic descriptor scope differs"));
        }
        let fingerprint = descriptor.fingerprint()?;
        let project = scope
            .project_id
            .ok_or_else(|| AppError::invalid_request("project scope required"))?;
        let mut tx = self.transaction(scope).await?;
        Self::lock_project(&mut tx, scope).await?;
        let active: bool = sqlx::query_scalar(
            "SELECT status='active' FROM projects WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3",
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
            .fetch_one(&mut *tx).await.map_err(db)?;
        if !active {
            return Err(AppError::conflict(
                "content reuse requires an active project",
            ));
        }
        let json: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT state FROM content_executions WHERE operator_id=$1 AND tenant_id=$2 \
             AND project_id=$3 AND execution_id=$4 FOR UPDATE",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project.as_uuid())
        .bind(request.execution_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        let mut state =
            decode(json.ok_or_else(|| AppError::not_found("content execution not found"))?)?;
        if state.execution.status != geo_domain::ContentExecutionStatus::Running {
            return Err(AppError::conflict("execution is not running"));
        }
        let item = state
            .items
            .iter()
            .find(|i| i.item_id == request.item_id)
            .ok_or_else(|| AppError::not_found("content item not found"))?
            .clone();
        if item.document_key != descriptor.document_key
            || item
                .source_version_refs
                .iter()
                .collect::<std::collections::HashSet<_>>()
                != descriptor
                    .source_version_ids
                    .iter()
                    .collect::<std::collections::HashSet<_>>()
        {
            return Err(AppError::conflict(
                "descriptor differs from frozen document inputs",
            ));
        }
        Self::validate_reuse_sources(&mut tx, scope, request.execution_id, &descriptor).await?;
        if item.status == ContentItemStatus::Ready {
            if item.semantic_fingerprint.as_deref() != Some(&fingerprint) {
                return Err(AppError::conflict(
                    "ready item has different semantic input",
                ));
            }
            tx.commit().await.map_err(db)?;
            return Ok(ContentReuseDecision::Ready(item));
        }
        if item.status != ContentItemStatus::Pending {
            if item.semantic_fingerprint.as_deref() != Some(&fingerprint) {
                return Err(AppError::conflict(
                    "item already has a different semantic input",
                ));
            }
            tx.commit().await.map_err(db)?;
            return Ok(ContentReuseDecision::Busy(item));
        }
        sqlx::query("INSERT INTO content_reuse_registry (operator_id,tenant_id,project_id,fingerprint,descriptor) \
             VALUES ($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
            .bind(&fingerprint).bind(serde_json::to_value(&descriptor).map_err(encode)?)
            .execute(&mut *tx).await.map_err(db)?;
        let registry = sqlx::query(
            "SELECT descriptor,origin_execution_id,origin_item_id,asset_id,revision_id,check_id, \
             reservation_execution_id,reservation_item_id,reservation_token,reservation_expires_at \
             FROM content_reuse_registry WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 \
             AND fingerprint=$4 FOR UPDATE",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project.as_uuid())
        .bind(&fingerprint)
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        if registry.get::<serde_json::Value, _>("descriptor")
            != serde_json::to_value(&descriptor).map_err(encode)?
        {
            return Err(AppError::conflict(
                "semantic fingerprint descriptor collision",
            ));
        }
        if let (
            Some(origin_execution_id),
            Some(origin_item_id),
            Some(asset_id),
            Some(revision_id),
            Some(check_id),
        ) = (
            registry.get::<Option<Uuid>, _>("origin_execution_id"),
            registry.get::<Option<Uuid>, _>("origin_item_id"),
            registry.get::<Option<Uuid>, _>("asset_id"),
            registry.get::<Option<Uuid>, _>("revision_id"),
            registry.get::<Option<Uuid>, _>("check_id"),
        ) {
            let origin_json: Option<serde_json::Value> = sqlx::query_scalar(
                "SELECT state FROM content_executions WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 \
                 AND execution_id=$4",
            ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
                .bind(origin_execution_id).fetch_optional(&mut *tx).await.map_err(db)?;
            let origin = decode(
                origin_json.ok_or_else(|| AppError::conflict("reuse origin is unavailable"))?,
            )?;
            let origin_item = origin.items.iter().find(|i| i.item_id == origin_item_id);
            let valid = origin_item.is_some_and(|i| {
                i.status == ContentItemStatus::Ready
                    && i.asset_id == Some(asset_id)
                    && i.ready_revision_id == Some(revision_id)
                    && i.semantic_fingerprint.as_deref() == Some(&fingerprint)
            }) && origin.checks.iter().any(|c| {
                c.check_id == check_id
                    && c.revision_id == revision_id
                    && !c.findings.iter().any(|f| f.blocking)
            }) && origin
                .assets
                .iter()
                .any(|a| a.asset_id == asset_id && a.current_revision_id == revision_id);
            if valid {
                let binding = ContentReuseBinding {
                    origin_execution_id,
                    origin_item_id,
                    asset_id,
                    revision_id,
                    check_id,
                    fingerprint: fingerprint.clone(),
                    reused_at: request.now,
                };
                let ready = state.apply_reuse(
                    request.item_id,
                    binding.clone(),
                    descriptor.clone(),
                    &fingerprint,
                )?;
                sqlx::query("INSERT INTO content_reuse_bindings \
                     (operator_id,tenant_id,project_id,execution_id,item_id,fingerprint,origin_execution_id,origin_item_id,asset_id,revision_id,check_id,reused_at) \
                     VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)")
                    .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
                    .bind(request.execution_id).bind(request.item_id).bind(&fingerprint)
                    .bind(origin_execution_id).bind(origin_item_id).bind(asset_id).bind(revision_id)
                    .bind(check_id).bind(request.now).execute(&mut *tx).await.map_err(db)?;
                sqlx::query("UPDATE content_executions SET state=$1 WHERE execution_id=$2")
                    .bind(serde_json::to_value(&state).map_err(encode)?)
                    .bind(request.execution_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
                tx.commit().await.map_err(db)?;
                return Ok(ContentReuseDecision::Ready(ready));
            }
            sqlx::query(
                "UPDATE content_reuse_registry SET origin_execution_id=NULL,origin_item_id=NULL,\
                 asset_id=NULL,revision_id=NULL,check_id=NULL,updated_at=now() \
                 WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND fingerprint=$4",
            )
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project.as_uuid())
            .bind(&fingerprint)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        }
        let historical: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM content_reuse_legacy_branches \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 \
             AND document_key=$4 AND source_version_refs @> $5 AND source_version_refs <@ $5)",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project.as_uuid())
        .bind(&descriptor.document_key)
        .bind(serde_json::to_value(&descriptor.source_version_ids).map_err(encode)?)
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        if historical {
            let blocked = state.classify(
                request.item_id,
                ContentItemStatus::Blocked,
                "reuse_provenance_insufficient",
            )?;
            sqlx::query("UPDATE content_executions SET state=$1 WHERE execution_id=$2")
                .bind(serde_json::to_value(&state).map_err(encode)?)
                .bind(request.execution_id)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            tx.commit().await.map_err(db)?;
            return Ok(ContentReuseDecision::InsufficientEvidence(blocked));
        }
        if registry
            .get::<Option<DateTime<Utc>>, _>("reservation_expires_at")
            .is_some_and(|expires| expires > request.now)
        {
            tx.commit().await.map_err(db)?;
            return Ok(ContentReuseDecision::Busy(item));
        }
        let lease = state.claim(
            request.item_id,
            ContentStep::Prepare,
            &request.owner,
            request.now,
            request.ttl_seconds,
        )?;
        state.set_semantic_descriptor(&lease, descriptor.clone(), &fingerprint)?;
        sqlx::query(
            "UPDATE content_reuse_registry SET reservation_execution_id=$1,reservation_item_id=$2, \
             reservation_token=$3,reservation_expires_at=$4,updated_at=now() \
             WHERE operator_id=$5 AND tenant_id=$6 AND project_id=$7 AND fingerprint=$8",
        )
        .bind(request.execution_id)
        .bind(request.item_id)
        .bind(lease.token)
        .bind(lease.expires_at)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project.as_uuid())
        .bind(&fingerprint)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        sqlx::query("UPDATE content_executions SET state=$1 WHERE execution_id=$2")
            .bind(serde_json::to_value(&state).map_err(encode)?)
            .bind(request.execution_id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        let item = state
            .items
            .iter()
            .find(|i| i.item_id == request.item_id)
            .expect("claimed item")
            .clone();
        tx.commit().await.map_err(db)?;
        Ok(ContentReuseDecision::Reserved { item, lease })
    }
    async fn resolve_checked_revision(
        &self,
        scope: &TenantScope,
        execution_id: Uuid,
        item_id: Uuid,
        revision_id: Uuid,
    ) -> Result<Option<ContentRevision>, AppError> {
        let Some(state) = self.read(scope, execution_id).await? else {
            return Ok(None);
        };
        if !state.items.iter().any(|i| i.item_id == item_id) {
            return Ok(None);
        }
        let Some(project) = scope.project_id else {
            return Err(AppError::invalid_request("project scope required"));
        };
        let binding: Option<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT origin_execution_id,check_id FROM content_reuse_bindings WHERE \
             operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND execution_id=$4 \
             AND item_id=$5 AND revision_id=$6",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project.as_uuid())
        .bind(execution_id)
        .bind(item_id)
        .bind(revision_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        if let Some((origin_execution_id, check_id)) = binding {
            let row: Option<serde_json::Value> = sqlx::query_scalar(
                "SELECT r.body FROM content_revisions r JOIN content_checks c \
                 ON c.operator_id=r.operator_id AND c.tenant_id=r.tenant_id AND c.project_id=r.project_id \
                 AND c.revision_id=r.revision_id WHERE r.operator_id=$1 AND r.tenant_id=$2 \
                 AND r.project_id=$3 AND r.execution_id=$4 AND r.revision_id=$5 AND c.check_id=$6 \
                 AND NOT EXISTS (SELECT 1 FROM jsonb_array_elements(c.body->'findings') f WHERE (f->>'blocking')::boolean)",
            ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
                .bind(origin_execution_id).bind(revision_id).bind(check_id)
                .fetch_optional(&self.pool).await.map_err(db)?;
            return row
                .map(|v| {
                    serde_json::from_value(v).map_err(|_| {
                        AppError::new(ErrorCode::Internal, "stored origin revision invalid")
                    })
                })
                .transpose();
        }
        if !state.assets.iter().any(|a| {
            a.item_id == item_id
                && state
                    .revisions
                    .iter()
                    .any(|r| r.revision_id == revision_id && r.asset_id == a.asset_id)
        }) || !state.checks.iter().any(|c| {
            c.revision_id == revision_id && !c.findings.iter().any(|finding| finding.blocking)
        }) {
            return Ok(None);
        }
        Ok(state
            .revisions
            .into_iter()
            .find(|r| r.revision_id == revision_id))
    }
    async fn fork_reused_item(
        &self,
        scope: &TenantScope,
        execution_id: Uuid,
        item_id: Uuid,
        base_revision_id: Uuid,
        document: StructuredDocument,
    ) -> Result<ContentRevision, AppError> {
        let origin = self
            .resolve_checked_revision(scope, execution_id, item_id, base_revision_id)
            .await?
            .ok_or_else(|| AppError::conflict("checked origin revision missing"))?;
        self.mutate(scope, execution_id, |s| {
            s.fork_reused_item(item_id, base_revision_id, &origin, document)
        })
        .await
    }
    async fn start(
        &self,
        scope: &TenantScope,
        cycle_id: Uuid,
        manifest: DocumentManifest,
        policy_version: &str,
    ) -> Result<ContentExecution, AppError> {
        // Never trust a caller-supplied manifest for persisted fan-out input.
        let canonical = geo_domain::KnowledgeRepository::get_document_manifest(
            &crate::PgKnowledgeRepository::new(self.pool.clone()),
            scope,
            manifest.manifest_id,
        )
        .await?
        .ok_or_else(|| AppError::not_found("document manifest not found"))?;
        if canonical != manifest {
            return Err(AppError::conflict("manifest differs from frozen storage"));
        }
        let state = start_content_state(scope, cycle_id, &manifest, policy_version)?;
        let Some(project) = scope.project_id else {
            return Err(AppError::invalid_request("project scope required"));
        };
        let mut tx = self.transaction(scope).await?;
        // Serializes bootstrap with schedule_next_cycle and project pause.
        // Service-level validation alone is insufficient across those calls.
        let current: Option<(String, Option<Uuid>)> = sqlx::query_as(
            "SELECT status,current_cycle_id FROM projects \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 FOR UPDATE",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        if !matches!(current, Some((ref status, Some(id))) if status == "active" && id == cycle_id)
        {
            return Err(AppError::conflict(
                "content start requires an active current cycle",
            ));
        }
        let linked: Option<i32> = sqlx::query_scalar(
            "SELECT revision FROM document_manifests WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND cycle_id=$4 AND manifest_id=$5 AND sealed=true",
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid()).bind(cycle_id)
            .bind(manifest.manifest_id).fetch_optional(&mut *tx).await.map_err(db)?;
        if linked != Some(manifest.revision) {
            return Err(AppError::conflict("manifest is not sealed for cycle"));
        }
        sqlx::query(
            "INSERT INTO content_executions (execution_id,operator_id,tenant_id,project_id,cycle_id,manifest_id,manifest_revision,policy_version,input_hash,state) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT DO NOTHING",
        ).bind(state.execution.execution_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
            .bind(cycle_id).bind(manifest.manifest_id).bind(manifest.revision).bind(policy_version)
            .bind(&state.execution.input_hash).bind(serde_json::to_value(&state).map_err(encode)?)
            .execute(&mut *tx).await.map_err(db)?;
        let stored_json: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT state FROM content_executions WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND cycle_id=$4 AND manifest_id=$5 AND manifest_revision=$6 AND policy_version=$7",
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
            .bind(cycle_id).bind(manifest.manifest_id).bind(manifest.revision).bind(policy_version)
            .fetch_optional(&mut *tx).await.map_err(db)?;
        let stored =
            decode(stored_json.ok_or_else(|| AppError::conflict("execution key conflicts"))?)?;
        if stored.execution.input_hash != state.execution.input_hash
            || stored
                .items
                .iter()
                .map(|i| &i.input_hash)
                .collect::<Vec<_>>()
                != state
                    .items
                    .iter()
                    .map(|i| &i.input_hash)
                    .collect::<Vec<_>>()
        {
            return Err(AppError::conflict("content execution replay inputs differ"));
        }
        tx.commit().await.map_err(db)?;
        Ok(stored.execution)
    }
    async fn get_execution(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Option<ContentExecution>, AppError> {
        Ok(self.read(scope, id).await?.map(|s| s.execution))
    }
    async fn get_handoff(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Option<ContentHandoff>, AppError> {
        Ok(self.read(scope, id).await?.and_then(|s| s.handoff))
    }
    async fn list_handoffs(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Vec<ContentHandoff>, AppError> {
        Ok(self
            .read(scope, id)
            .await?
            .map(|s| s.handoffs)
            .unwrap_or_default())
    }
    async fn list_executions(
        &self,
        scope: &TenantScope,
        cycle_id: Uuid,
    ) -> Result<Vec<ContentExecution>, AppError> {
        let Some(project) = scope.project_id else {
            return Err(AppError::invalid_request("project scope required"));
        };
        let mut tx = self.transaction(scope).await?;
        let rows:Vec<serde_json::Value>=sqlx::query_scalar("SELECT state FROM content_executions WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND cycle_id=$4 ORDER BY created_at,execution_id")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid()).bind(cycle_id)
            .fetch_all(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        rows.into_iter()
            .map(|v| decode(v).map(|s| s.execution))
            .collect()
    }
    async fn get_item(
        &self,
        scope: &TenantScope,
        id: Uuid,
        item: Uuid,
    ) -> Result<Option<ContentItem>, AppError> {
        Ok(self
            .read(scope, id)
            .await?
            .and_then(|s| s.items.into_iter().find(|i| i.item_id == item)))
    }
    async fn list_items(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Vec<ContentItem>, AppError> {
        Ok(self
            .read(scope, id)
            .await?
            .map(|s| s.items)
            .unwrap_or_default())
    }
    async fn get_asset(
        &self,
        scope: &TenantScope,
        asset: Uuid,
    ) -> Result<Option<ContentAsset>, AppError> {
        let id = match self.asset_execution(scope, asset).await {
            Ok(id) => id,
            Err(error) if error.code == ErrorCode::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(self
            .read(scope, id)
            .await?
            .and_then(|s| s.assets.into_iter().find(|a| a.asset_id == asset)))
    }
    async fn list_assets(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Vec<ContentAsset>, AppError> {
        Ok(self
            .read(scope, id)
            .await?
            .map(|s| s.assets)
            .unwrap_or_default())
    }
    async fn list_project_assets(
        &self,
        scope: &TenantScope,
    ) -> Result<Vec<ContentAsset>, AppError> {
        let Some(project) = scope.project_id else {
            return Err(AppError::invalid_request("project scope required"));
        };
        let mut tx = self.transaction(scope).await?;
        let rows:Vec<serde_json::Value>=sqlx::query_scalar("SELECT state FROM content_executions WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 ORDER BY created_at,execution_id")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid()).fetch_all(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(rows
            .into_iter()
            .map(decode)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flat_map(|s| s.assets)
            .collect())
    }
    async fn list_revisions(
        &self,
        scope: &TenantScope,
        asset: Uuid,
    ) -> Result<Vec<ContentRevision>, AppError> {
        let Some(project) = scope.project_id else {
            return Err(AppError::invalid_request("project scope required"));
        };
        let mut tx = self.transaction(scope).await?;
        let rows:Vec<serde_json::Value>=sqlx::query_scalar("SELECT body FROM content_revisions WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND asset_id=$4 ORDER BY revision")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid()).bind(asset)
            .fetch_all(&mut *tx).await.map_err(db)?;
        let checks:Vec<serde_json::Value>=sqlx::query_scalar(
            "SELECT check_record.body FROM content_checks check_record JOIN content_revisions revision ON revision.revision_id=check_record.revision_id WHERE revision.operator_id=$1 AND revision.tenant_id=$2 AND revision.project_id=$3 AND revision.asset_id=$4"
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid()).bind(asset)
            .fetch_all(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        let checks: Vec<geo_domain::ContentCheck> = checks
            .into_iter()
            .map(|v| {
                serde_json::from_value(v)
                    .map_err(|_| AppError::new(ErrorCode::Internal, "stored content check invalid"))
            })
            .collect::<Result<_, _>>()?;
        let mut revisions: Vec<ContentRevision> = rows
            .into_iter()
            .map(|v| {
                serde_json::from_value(v).map_err(|_| {
                    AppError::new(ErrorCode::Internal, "stored content revision invalid")
                })
            })
            .collect::<Result<_, _>>()?;
        for revision in &mut revisions {
            if let Some(check) = checks
                .iter()
                .find(|c| c.revision_id == revision.revision_id)
            {
                revision.findings = check.findings.clone();
            }
        }
        Ok(revisions)
    }
    async fn list_checks(
        &self,
        scope: &TenantScope,
        revision_id: Uuid,
    ) -> Result<Vec<ContentCheck>, AppError> {
        let Some(project) = scope.project_id else {
            return Err(AppError::invalid_request("project scope required"));
        };
        let mut tx = self.transaction(scope).await?;
        let rows: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT body FROM content_checks WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND revision_id=$4 ORDER BY created_at,check_id",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project.as_uuid())
        .bind(revision_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        rows.into_iter()
            .map(|value| {
                serde_json::from_value(value)
                    .map_err(|_| AppError::new(ErrorCode::Internal, "stored content check invalid"))
            })
            .collect()
    }
    async fn claim(
        &self,
        scope: &TenantScope,
        id: Uuid,
        item: Uuid,
        step: ContentStep,
        owner: &str,
        now: DateTime<Utc>,
        ttl: i64,
    ) -> Result<StepLease, AppError> {
        self.mutate(scope, id, |s| s.claim(item, step, owner, now, ttl))
            .await
    }
    async fn complete_prepare(
        &self,
        scope: &TenantScope,
        lease: &StepLease,
        brief: ContentBrief,
    ) -> Result<ContentItem, AppError> {
        self.mutate(scope, lease.execution_id, |s| {
            s.complete_prepare(lease, brief)
        })
        .await
    }
    async fn complete_generate(
        &self,
        scope: &TenantScope,
        lease: &StepLease,
        doc: StructuredDocument,
    ) -> Result<ContentRevision, AppError> {
        self.mutate(scope, lease.execution_id, |s| {
            s.complete_generate(lease, doc)
        })
        .await
    }
    async fn complete_check(
        &self,
        scope: &TenantScope,
        lease: &StepLease,
        findings: Vec<ContentFinding>,
    ) -> Result<ContentItem, AppError> {
        self.mutate(scope, lease.execution_id, |s| {
            s.complete_check(lease, findings)
        })
        .await
    }
    async fn complete_repair(
        &self,
        scope: &TenantScope,
        lease: &StepLease,
        document: StructuredDocument,
    ) -> Result<ContentRevision, AppError> {
        self.mutate(scope, lease.execution_id, |s| {
            s.complete_repair(lease, document)
        })
        .await
    }
    async fn edit(
        &self,
        scope: &TenantScope,
        asset: Uuid,
        base: Uuid,
        doc: StructuredDocument,
    ) -> Result<ContentRevision, AppError> {
        let id = self.asset_execution(scope, asset).await?;
        self.mutate(scope, id, |s| s.edit(asset, base, doc)).await
    }
    async fn classify(
        &self,
        scope: &TenantScope,
        id: Uuid,
        item: Uuid,
        status: ContentItemStatus,
        reason: &str,
    ) -> Result<ContentItem, AppError> {
        self.mutate(scope, id, |s| s.classify(item, status, reason))
            .await
    }
    async fn fail_step(
        &self,
        scope: &TenantScope,
        lease: &StepLease,
        reason: &str,
    ) -> Result<ContentItem, AppError> {
        self.mutate(scope, lease.execution_id, |s| s.fail_step(lease, reason))
            .await
    }
    async fn release_step(
        &self,
        scope: &TenantScope,
        lease: &StepLease,
    ) -> Result<ContentItem, AppError> {
        self.mutate(scope, lease.execution_id, |s| s.release_step(lease))
            .await
    }
    async fn invalidate_ready(
        &self,
        scope: &TenantScope,
        id: Uuid,
        item: Uuid,
        reason: &str,
    ) -> Result<ContentItem, AppError> {
        self.mutate(scope, id, |s| s.invalidate_ready(item, reason))
            .await
    }
    async fn close(&self, scope: &TenantScope, id: Uuid) -> Result<ContentHandoff, AppError> {
        self.mutate(scope, id, ContentState::close).await
    }
    async fn cancel(&self, scope: &TenantScope, id: Uuid) -> Result<ContentExecution, AppError> {
        self.mutate(scope, id, ContentState::cancel).await
    }
}
