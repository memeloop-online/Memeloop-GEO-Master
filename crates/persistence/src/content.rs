use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, ContentAsset, ContentBrief, ContentExecution, ContentFinding, ContentHandoff,
    ContentItem, ContentItemStatus, ContentRepository, ContentRevision, ContentState, ContentStep,
    DocumentManifest, ErrorCode, StepLease, StructuredDocument, TenantScope, start_content_state,
};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

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
        if state.execution.status != geo_domain::ContentExecutionStatus::Running
            || row
                .get::<Option<DateTime<Utc>>, _>("dispatch_expires_at")
                .is_some_and(|t| t > now)
            || row
                .get::<Option<DateTime<Utc>>, _>("dispatch_retry_after")
                .is_some_and(|t| t > now)
        {
            return Ok(None);
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
               AND state->'execution'->>'status'='running' \
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
        let json: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT state FROM content_executions WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND execution_id=$4 FOR UPDATE",
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
            .bind(id).fetch_optional(&mut *tx).await.map_err(db)?;
        let mut state =
            decode(json.ok_or_else(|| AppError::not_found("content execution not found"))?)?;
        let previous_briefs: Vec<Uuid> = state
            .items
            .iter()
            .filter(|i| i.brief.is_some())
            .map(|i| i.item_id)
            .collect();
        let previous_revision_count = state.revisions.len();
        let previous_check_count = state.checks.len();
        let previous_handoff_count = state.handoffs.len();
        let result = change(&mut state)?;
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
            sqlx::query("INSERT INTO content_revisions (revision_id,operator_id,tenant_id,project_id,execution_id,asset_id,revision,body,created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)")
                .bind(revision.revision_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
                .bind(id).bind(revision.asset_id).bind(revision.revision)
                .bind(serde_json::to_value(revision).map_err(encode)?).bind(revision.created_at)
                .execute(&mut *tx).await.map_err(db)?;
        }
        for check in &state.checks[previous_check_count..] {
            sqlx::query("INSERT INTO content_checks (check_id,operator_id,tenant_id,project_id,execution_id,revision_id,body,created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)")
                .bind(check.check_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
                .bind(id).bind(check.revision_id).bind(serde_json::to_value(check).map_err(encode)?).bind(check.created_at)
                .execute(&mut *tx).await.map_err(db)?;
        }
        for handoff in &state.handoffs[previous_handoff_count..] {
            sqlx::query("INSERT INTO content_handoffs (handoff_id,operator_id,tenant_id,project_id,execution_id,revision,supersedes_handoff_id,body,created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)")
                    .bind(handoff.handoff_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
                    .bind(id).bind(handoff.revision).bind(handoff.supersedes_handoff_id)
                    .bind(serde_json::to_value(handoff).map_err(encode)?).bind(handoff.created_at)
                    .execute(&mut *tx).await.map_err(db)?;
        }
        sqlx::query("UPDATE content_executions SET state=$1 WHERE operator_id=$2 AND tenant_id=$3 AND project_id=$4 AND execution_id=$5")
            .bind(serde_json::to_value(state).map_err(encode)?).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
            .bind(project.as_uuid()).bind(id).execute(&mut *tx).await.map_err(db)?;
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
