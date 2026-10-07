use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, ChannelOutcome, ChannelOutcomeStatus, ChannelVariant, ContentCheck, ContentExecution,
    ContentHandoff, ContentRevision, DistributionCycleInputs, DistributionDeferralReason,
    DistributionExpansionPage, DistributionManifest, DistributionPublicationResult,
    DistributionRepository, DistributionSnapshot, DistributionTarget, DistributionTargetPage,
    DistributionTargetStatus, ErrorCode, FreezeDistribution, IntentVerification,
    MaterializedDistribution, PreparedDistribution, PublicationBundle, PublicationCommand,
    PublicationIntent, PublicationOrigin, ReportPublicationStatus, TenantScope, distribution_cell,
    freeze_distribution, prepare_publication_intent, prepare_variant, target_publication_evidence,
};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

#[derive(Clone)]
pub struct PgDistributionRepository {
    pool: PgPool,
}

impl PgDistributionRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub fn from_database(database: &crate::Database) -> Self {
        Self::new(database.pool().clone())
    }
    async fn transaction<'a>(
        &'a self,
        scope: &TenantScope,
    ) -> Result<Transaction<'a, Postgres>, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        crate::set_local_scope(&mut tx, scope).await.map_err(db)?;
        Ok(tx)
    }
}

fn db(error: sqlx::Error) -> AppError {
    AppError::new(
        ErrorCode::DependencyUnavailable,
        format!("distribution database operation failed: {error}"),
    )
}
fn encode<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, AppError> {
    serde_json::to_value(value)
        .map_err(|_| AppError::new(ErrorCode::Internal, "cannot encode distribution value"))
}
fn decode<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, AppError> {
    serde_json::from_value(value)
        .map_err(|_| AppError::new(ErrorCode::Internal, "stored distribution value invalid"))
}
fn project(scope: &TenantScope) -> Result<Uuid, AppError> {
    scope
        .project_id
        .map(|id| id.as_uuid())
        .ok_or_else(|| AppError::forbidden("distribution requires project scope"))
}
fn from_row(row: &sqlx::postgres::PgRow) -> Result<DistributionManifest, AppError> {
    let mut manifest: DistributionManifest = decode(row.get("frozen"))?;
    manifest.expansion_cursor = u64::try_from(row.get::<i64, _>("expansion_cursor"))
        .map_err(|_| AppError::new(ErrorCode::Internal, "invalid expansion cursor"))?;
    manifest.complete = row.get("complete");
    manifest.sealed_at = row.get("sealed_at");
    Ok(manifest)
}
async fn locked_manifest(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    id: Uuid,
) -> Result<DistributionManifest, AppError> {
    let row = sqlx::query(
        "SELECT frozen,expansion_cursor,complete,sealed_at FROM distribution_execution_manifests \
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND manifest_id=$4 FOR UPDATE",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project(scope)?)
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db)?
    .ok_or_else(|| AppError::not_found("distribution manifest not found"))?;
    from_row(&row)
}
async fn current_target(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    id: Uuid,
) -> Result<DistributionTarget, AppError> {
    let row: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT current_body FROM distribution_execution_targets WHERE operator_id=$1 \
         AND tenant_id=$2 AND project_id=$3 AND target_id=$4 FOR UPDATE",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project(scope)?)
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db)?;
    decode(row.ok_or_else(|| AppError::not_found("distribution target not found"))?)
}
/// Check the original frozen document item's complete source dependency set
/// and the exact checked revision quotes under source row locks. This runs
/// after the project lock and before creating a variant, intent or command.
async fn live_materialization_sources(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    manifest: &DistributionManifest,
    target: &DistributionTarget,
    revision: &ContentRevision,
) -> Result<Option<DistributionDeferralReason>, AppError> {
    let row = sqlx::query(
        "SELECT mi.knowledge_release_id,mi.source_version_refs \
         FROM document_manifest_items mi JOIN document_manifests m ON \
           (m.operator_id,m.tenant_id,m.project_id,m.manifest_id)= \
           (mi.operator_id,mi.tenant_id,mi.project_id,mi.manifest_id) \
         WHERE mi.operator_id=$1 AND mi.tenant_id=$2 AND mi.project_id=$3 \
           AND mi.manifest_id=$4 AND mi.document_manifest_item_id=$5 \
           AND m.revision=$6 AND m.sealed=true",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project(scope)?)
    .bind(manifest.document_manifest_id)
    .bind(target.document_item_id)
    .bind(manifest.document_manifest_revision)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db)?;
    let Some(row) = row else {
        return Ok(Some(DistributionDeferralReason::SourceUnavailable));
    };
    let release: Uuid = row.get("knowledge_release_id");
    let source_ids: Vec<Uuid> = decode(row.get("source_version_refs"))?;
    if source_ids.is_empty() || revision.evidence.is_empty() {
        return Ok(Some(DistributionDeferralReason::SourceUnavailable));
    }
    for source_version_id in &source_ids {
        let row = sqlx::query(
            "SELECT s.state,s.purpose,s.current_version_id FROM knowledge_source_versions v \
             JOIN knowledge_sources s ON (s.operator_id,s.tenant_id,s.project_id,s.source_id)= \
               (v.operator_id,v.tenant_id,v.project_id,v.source_id) \
             JOIN knowledge_release_source_versions lr ON \
               (lr.operator_id,lr.tenant_id,lr.project_id,lr.source_version_id)= \
               (v.operator_id,v.tenant_id,v.project_id,v.source_version_id) \
             WHERE v.operator_id=$1 AND v.tenant_id=$2 AND v.project_id=$3 \
               AND v.source_version_id=$4 AND lr.knowledge_release_id=$5 FOR SHARE OF s",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(*source_version_id)
        .bind(release)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db)?;
        let Some(row) = row else {
            return Ok(Some(DistributionDeferralReason::SourceUnavailable));
        };
        if row.get::<Option<Uuid>, _>("current_version_id") != Some(*source_version_id) {
            return Ok(Some(DistributionDeferralReason::SourceChanged));
        }
        if row.get::<String, _>("state") != "active" || row.get::<String, _>("purpose") != "public"
        {
            return Ok(Some(DistributionDeferralReason::SourceUnavailable));
        }
    }
    for reference in &revision.evidence {
        if !source_ids.contains(&reference.source_version_id) {
            return Ok(Some(DistributionDeferralReason::SourceUnavailable));
        }
        let Some(quote) = revision
            .quotes
            .iter()
            .find(|quote| quote.reference == *reference)
        else {
            return Ok(Some(DistributionDeferralReason::SourceUnavailable));
        };
        let Some(chunk) = reference.chunk_id else {
            return Ok(Some(DistributionDeferralReason::SourceUnavailable));
        };
        let row = sqlx::query(
            "SELECT text,locator FROM knowledge_chunks WHERE operator_id=$1 AND tenant_id=$2 \
             AND project_id=$3 AND source_version_id=$4 AND chunk_id=$5",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(reference.source_version_id)
        .bind(chunk)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db)?;
        let Some(row) = row else {
            return Ok(Some(DistributionDeferralReason::SourceUnavailable));
        };
        let text: String = row.get("text");
        let expected = if matches!(reference.locator, geo_domain::ChunkLocator::Csv { .. }) {
            text
        } else {
            text.chars().take(1600).collect()
        };
        if row.get::<serde_json::Value, _>("locator") != encode(&reference.locator)?
            || expected != quote.exact_quote
            || quote.exact_quote.chars().count() > 1600
        {
            return Ok(Some(DistributionDeferralReason::SourceUnavailable));
        }
    }
    Ok(None)
}
async fn change_target(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    old: &DistributionTarget,
    mut next: DistributionTarget,
) -> Result<DistributionTarget, AppError> {
    if *old != next {
        next.version = old
            .version
            .checked_add(1)
            .ok_or_else(|| AppError::invalid_request("target version overflow"))?;
        let updated = sqlx::query(
            "UPDATE distribution_execution_targets SET current_version=$1,current_body=$2 \
             WHERE operator_id=$3 AND tenant_id=$4 AND project_id=$5 AND target_id=$6 AND current_version=$7"
        ).bind(next.version as i64).bind(encode(&next)?).bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid()).bind(project(scope)?).bind(next.target_id)
            .bind(old.version as i64).execute(&mut **tx).await.map_err(db)?;
        if updated.rows_affected() != 1 {
            return Err(AppError::conflict("target version changed"));
        }
        sqlx::query(
            "INSERT INTO distribution_target_versions \
             (target_id,operator_id,tenant_id,project_id,version,body) VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(next.target_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(next.version as i64)
        .bind(encode(&next)?)
        .execute(&mut **tx)
        .await
        .map_err(db)?;
    }
    Ok(next)
}

#[async_trait]
impl DistributionRepository for PgDistributionRepository {
    async fn publication_results(
        &self,
        scope: &TenantScope,
        targets: &[DistributionTarget],
        at: DateTime<Utc>,
    ) -> Result<Vec<DistributionPublicationResult>, AppError> {
        let intent_ids: Vec<Uuid> = targets
            .iter()
            .filter_map(|target| target.publication_intent_id)
            .collect();
        if intent_ids.is_empty() {
            return Ok(vec![]);
        }
        let mut tx = self.transaction(scope).await?;
        // The historical target binding is supplied by as_of. The query never
        // reads the mutable current target body or current intent verification,
        // both of which can change after a report's original cutoff.
        let rows = sqlx::query(
            "SELECT i.intent_id,c.fixture AS command_fixture,a.claimed_at, \
                    e.evidence_id,e.result,e.fixture AS evidence_fixture,e.observed_at, \
                    e.external_receipt,e.public_readback,ca.attempt_id AS channel_attempt_id, \
                    ca.outcome,ca.received_at \
             FROM distribution_publication_intents i \
             JOIN distribution_publication_commands c ON \
               (c.operator_id,c.tenant_id,c.project_id,c.intent_id) = \
               (i.operator_id,i.tenant_id,i.project_id,i.intent_id) \
             LEFT JOIN distribution_publication_attempts a ON \
               (a.operator_id,a.tenant_id,a.project_id,a.intent_id) = \
               (i.operator_id,i.tenant_id,i.project_id,i.intent_id) AND a.claimed_at<=$5 \
             LEFT JOIN distribution_intent_evidence e ON \
               (e.operator_id,e.tenant_id,e.project_id,e.intent_id,e.attempt_id) = \
               (a.operator_id,a.tenant_id,a.project_id,a.intent_id,a.attempt_id) \
               AND e.observed_at<=$5 \
             LEFT JOIN channel_execution_attempts ca ON \
               (ca.operator_id,ca.tenant_id,ca.project_id,ca.attempt_id) = \
               (a.operator_id,a.tenant_id,a.project_id,a.attempt_id) \
               AND ca.target_id=c.materialized_target_id \
             WHERE i.operator_id=$1 AND i.tenant_id=$2 AND i.project_id=$3 \
               AND i.intent_id = ANY($4) \
             ORDER BY i.intent_id,e.observed_at,e.evidence_id",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(&intent_ids)
        .bind(at)
        .fetch_all(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;

        let mut by_intent = std::collections::HashMap::<
            Uuid,
            (
                ReportPublicationStatus,
                Option<Uuid>,
                Option<DateTime<Utc>>,
                Option<DateTime<Utc>>,
                Option<&'static str>,
            ),
        >::new();
        for row in rows {
            let intent_id: Uuid = row.get("intent_id");
            let Some(claimed_at) = row.get::<Option<DateTime<Utc>>, _>("claimed_at") else {
                continue;
            };
            let mut status = ReportPublicationStatus::Unknown;
            let mut receipt = None;
            let mut receipt_kind = None;
            let outcome_value: Option<serde_json::Value> = row.get("outcome");
            let received: Option<DateTime<Utc>> = row.get("received_at");
            let known = received.is_some_and(|time| time >= claimed_at && time <= at);
            let outcome: Option<ChannelOutcome> = if known {
                outcome_value.map(decode).transpose()?
            } else {
                None
            };
            let observed: Option<DateTime<Utc>> = row.get("observed_at");
            let evidence_id: Option<Uuid> = row.get("evidence_id");
            let valid_time = observed.is_some_and(|time| {
                time >= claimed_at && time <= at && received.is_some_and(|end| time <= end)
            });
            if let Some(outcome) = &outcome {
                let real = !row.get::<bool, _>("command_fixture") && !outcome.fixture;
                let outcome_time_valid = outcome.occurred_at >= claimed_at
                    && outcome.occurred_at <= at
                    && received.is_some_and(|end| outcome.occurred_at <= end);
                if real && outcome_time_valid {
                    status = match outcome.status {
                        ChannelOutcomeStatus::Failed => ReportPublicationStatus::Failed,
                        ChannelOutcomeStatus::LoginRequired | ChannelOutcomeStatus::Unsupported => {
                            ReportPublicationStatus::Deferred
                        }
                        _ => ReportPublicationStatus::Unknown,
                    };
                }
                let external: Option<serde_json::Value> = row.get("external_receipt");
                let readback: Option<serde_json::Value> = row.get("public_readback");
                if real
                    && outcome_time_valid
                    && valid_time
                    // TIMESTAMPTZ stores microseconds, while the immutable
                    // JSON outcome can retain chrono's nanoseconds.
                    && observed.is_some_and(|time| {
                        time.timestamp_micros() == outcome.occurred_at.timestamp_micros()
                    })
                    && row.get::<Option<String>, _>("result").as_deref() == Some("verified")
                    && !row
                        .get::<Option<bool>, _>("evidence_fixture")
                        .unwrap_or(true)
                    && outcome.status == ChannelOutcomeStatus::Verified
                    && external
                        .as_ref()
                        .and_then(|v| v.get("external_receipt_id"))
                        .and_then(|v| v.as_str())
                        .is_some()
                    && readback
                        .as_ref()
                        .and_then(|v| v.get("verified"))
                        .and_then(|v| v.as_bool())
                        == Some(true)
                    && readback
                        .as_ref()
                        .and_then(|v| v.get("public_url"))
                        .and_then(|v| v.as_str())
                        == outcome.public_url.as_deref()
                    && outcome.public_url.is_some()
                {
                    status = ReportPublicationStatus::Verified;
                    receipt = evidence_id;
                    receipt_kind = Some("public_verification");
                } else if real
                    && outcome_time_valid
                    && outcome.status == ChannelOutcomeStatus::Published
                    && outcome
                        .public_url
                        .as_deref()
                        .is_some_and(|url| !url.is_empty())
                    && row.get::<Option<Uuid>, _>("channel_attempt_id").is_some()
                {
                    status = ReportPublicationStatus::Published;
                    receipt = row.get("channel_attempt_id");
                    receipt_kind = Some("publication_receipt");
                }
            }
            let candidate = (
                status,
                receipt,
                if status == ReportPublicationStatus::Published {
                    outcome.as_ref().map(|outcome| outcome.occurred_at)
                } else {
                    observed
                },
                received,
                receipt_kind,
            );
            let prior = by_intent.entry(intent_id).or_insert(candidate);
            // A valid verified readback wins over the earlier pre-send unknown.
            // A completed non-verified result wins over that initial unknown.
            if matches!(
                status,
                ReportPublicationStatus::Verified | ReportPublicationStatus::Published
            ) || (prior.0 == ReportPublicationStatus::Unknown
                && matches!(
                    status,
                    ReportPublicationStatus::Failed | ReportPublicationStatus::Deferred
                ))
            {
                *prior = candidate;
            }
        }
        Ok(targets
            .iter()
            .filter_map(|target| {
                let intent_id = target.publication_intent_id?;
                let (status, receipt, observed, received, kind) = by_intent.get(&intent_id)?;
                Some(DistributionPublicationResult {
                    target_id: target.target_id,
                    status: *status,
                    reason: None,
                    evidence: match (receipt, observed, received, kind) {
                        (Some(id), Some(occurred), Some(received), Some(kind)) => {
                            vec![target_publication_evidence(
                                target.target_id,
                                intent_id,
                                *id,
                                kind,
                                *occurred,
                                *received,
                            )]
                        }
                        _ => vec![],
                    },
                })
            })
            .collect())
    }
    async fn get_publication_bundle(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
    ) -> Result<PublicationBundle, AppError> {
        let mut tx = self.transaction(scope).await?;
        let row = sqlx::query(
            "SELECT i.body AS intent_body,i.verification,i.verification_evidence_id, \
                    v.body AS variant_body,r.body AS revision_body, \
                    t.current_body AS target_body,c.command_id,c.origin_target_id,c.origin_request_id, \
                    c.payload_hash,c.fixture \
             FROM distribution_publication_intents i \
             JOIN distribution_channel_variants v ON (v.operator_id,v.tenant_id,v.project_id,v.variant_id) = \
                (i.operator_id,i.tenant_id,i.project_id,i.variant_id) \
             JOIN content_revisions r ON (r.operator_id,r.tenant_id,r.project_id,r.revision_id) = \
                (v.operator_id,v.tenant_id,v.project_id,v.content_revision_id) \
             JOIN distribution_publication_commands c ON (c.operator_id,c.tenant_id,c.project_id,c.intent_id) = \
                (i.operator_id,i.tenant_id,i.project_id,i.intent_id) \
             LEFT JOIN distribution_execution_targets t ON (t.operator_id,t.tenant_id,t.project_id,t.target_id) = \
                (c.operator_id,c.tenant_id,c.project_id,c.origin_target_id) \
             WHERE i.operator_id=$1 AND i.tenant_id=$2 AND i.project_id=$3 AND i.intent_id=$4",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(intent_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or_else(|| AppError::not_found("publication bundle not found"))?;
        let mut intent: PublicationIntent = decode(row.get("intent_body"))?;
        intent.verification = decode(serde_json::Value::String(row.get("verification")))?;
        intent.verification_evidence_id = row.get("verification_evidence_id");
        let origin_target_id: Option<Uuid> = row.get("origin_target_id");
        let origin_request_id: Option<Uuid> = row.get("origin_request_id");
        let origin = match (origin_target_id, origin_request_id) {
            (Some(_), None) => PublicationOrigin::CoverageTarget {
                target: decode(row.get::<serde_json::Value, _>("target_body"))?,
            },
            (None, Some(request_id)) => {
                let query = format!(
                    "SELECT {} FROM content_distribution_requests WHERE operator_id=$1 \
                     AND tenant_id=$2 AND project_id=$3 AND request_id=$4",
                    crate::content_distribution_request::REQUEST_COLUMNS
                );
                let request = sqlx::query(&query)
                    .bind(scope.operator_id.as_uuid())
                    .bind(scope.tenant_id.as_uuid())
                    .bind(project(scope)?)
                    .bind(request_id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(db)?
                    .ok_or_else(|| AppError::not_found("publication origin request not found"))?;
                PublicationOrigin::ContentRequest {
                    request: crate::content_distribution_request::read_request(&request),
                }
            }
            _ => return Err(AppError::conflict("publication origin columns differ")),
        };
        let bundle = PublicationBundle {
            revision: decode(row.get("revision_body"))?,
            variant: decode(row.get("variant_body"))?,
            origin,
            command: PublicationCommand {
                command_id: row.get("command_id"),
                intent_id,
                target_id: origin_target_id.unwrap_or(Uuid::nil()),
                payload_hash: row.get("payload_hash"),
                fixture: row.get("fixture"),
            },
            intent,
        };
        bundle.validate_origin()?;
        tx.commit().await.map_err(db)?;
        Ok(bundle)
    }

    async fn freeze(
        &self,
        scope: &TenantScope,
        input: FreezeDistribution,
    ) -> Result<DistributionManifest, AppError> {
        let mut frozen = freeze_distribution(scope, input.clone())?;
        // A frozen roster must be derived from the persisted planning and
        // handoff rows, not merely from caller-supplied copies of those rows.
        let document = geo_domain::KnowledgeRepository::get_document_manifest(
            &crate::PgKnowledgeRepository::new(self.pool.clone()),
            scope,
            input.document_manifest.manifest_id,
        )
        .await?
        .ok_or_else(|| AppError::not_found("document manifest not found"))?;
        if document != input.document_manifest {
            return Err(AppError::conflict(
                "document manifest differs from frozen storage",
            ));
        }
        let mut tx = self.transaction(scope).await?;
        let skeleton: Option<Uuid> = sqlx::query_scalar(
            "SELECT manifest_id FROM distribution_manifests WHERE operator_id=$1 AND tenant_id=$2 \
             AND project_id=$3 AND cycle_id=$4 AND document_manifest_id=$5 ORDER BY revision LIMIT 1"
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .bind(input.cycle_id).bind(document.manifest_id).fetch_optional(&mut *tx).await.map_err(db)?;
        let skeleton = skeleton
            .ok_or_else(|| AppError::conflict("distribution skeleton missing for cycle"))?;
        let document_revision: Option<i32> = sqlx::query_scalar(
            "SELECT revision FROM document_manifests WHERE operator_id=$1 AND tenant_id=$2 \
             AND project_id=$3 AND cycle_id=$4 AND manifest_id=$5 AND sealed=true",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(input.cycle_id)
        .bind(document.manifest_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        if document_revision != Some(document.revision) {
            return Err(AppError::conflict("document manifest not sealed in cycle"));
        }
        let execution: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT state->'execution' FROM content_executions WHERE operator_id=$1 AND tenant_id=$2 \
             AND project_id=$3 AND cycle_id=$4 AND manifest_id=$5 AND execution_id=$6"
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .bind(input.cycle_id).bind(document.manifest_id).bind(input.content_execution.execution_id)
            .fetch_optional(&mut *tx).await.map_err(db)?;
        let persisted_execution: ContentExecution =
            decode(execution.ok_or_else(|| AppError::not_found("content execution not found"))?)?;
        if persisted_execution != input.content_execution {
            return Err(AppError::conflict("content execution differs from storage"));
        }
        let handoff: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT body FROM content_handoffs WHERE operator_id=$1 AND tenant_id=$2 \
             AND project_id=$3 AND execution_id=$4 AND handoff_id=$5",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(input.content_execution.execution_id)
        .bind(input.content_handoff.handoff_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        let persisted_handoff: ContentHandoff =
            decode(handoff.ok_or_else(|| AppError::not_found("content handoff not found"))?)?;
        if persisted_handoff != input.content_handoff {
            return Err(AppError::conflict(
                "content handoff differs from immutable storage",
            ));
        }
        let expected_count = i64::try_from(frozen.expected_count).map_err(|_| {
            AppError::invalid_request("distribution coverage exceeds database capacity")
        })?;
        frozen.sealed_at = Utc::now();
        sqlx::query(
            "INSERT INTO distribution_execution_manifests \
             (manifest_id,operator_id,tenant_id,project_id,cycle_id,skeleton_manifest_id,document_manifest_id, \
             content_execution_id,content_handoff_id,revision,input_hash,frozen,expected_count,complete,sealed_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,clock_timestamp()) \
             ON CONFLICT DO NOTHING"
        ).bind(frozen.manifest_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
            .bind(project(scope)?).bind(frozen.cycle_id).bind(skeleton).bind(frozen.document_manifest_id)
            .bind(frozen.content_execution_id).bind(frozen.content_handoff_id).bind(frozen.revision)
            .bind(&frozen.input_hash).bind(encode(&frozen)?).bind(expected_count)
            .bind(frozen.complete).execute(&mut *tx).await.map_err(db)?;
        let row = sqlx::query(
            "SELECT frozen,expansion_cursor,complete,sealed_at FROM distribution_execution_manifests \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND cycle_id=$4 AND revision=$5"
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .bind(frozen.cycle_id).bind(frozen.revision).fetch_optional(&mut *tx).await.map_err(db)?
            .ok_or_else(|| AppError::conflict("frozen distribution revision conflicts"))?;
        let stored = from_row(&row)?;
        if stored.manifest_id != frozen.manifest_id || stored.input_hash != frozen.input_hash {
            return Err(AppError::conflict("frozen distribution inputs differ"));
        }
        tx.commit().await.map_err(db)?;
        Ok(stored)
    }

    async fn get(&self, scope: &TenantScope, id: Uuid) -> Result<DistributionManifest, AppError> {
        let mut tx = self.transaction(scope).await?;
        let row = sqlx::query(
            "SELECT frozen,expansion_cursor,complete,sealed_at FROM distribution_execution_manifests \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND manifest_id=$4"
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
            .bind(project(scope)?).bind(id).fetch_optional(&mut *tx).await.map_err(db)?
            .ok_or_else(|| AppError::not_found("distribution manifest not found"))?;
        tx.commit().await.map_err(db)?;
        from_row(&row)
    }

    async fn get_target(
        &self,
        scope: &TenantScope,
        manifest_id: Uuid,
        target_id: Uuid,
    ) -> Result<DistributionTarget, AppError> {
        let mut tx = self.transaction(scope).await?;
        let body: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT current_body FROM distribution_execution_targets WHERE operator_id=$1 \
             AND tenant_id=$2 AND project_id=$3 AND manifest_id=$4 AND target_id=$5",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(manifest_id)
        .bind(target_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        decode(body.ok_or_else(|| AppError::not_found("distribution target not found"))?)
    }

    async fn latest_for_cycle(
        &self,
        scope: &TenantScope,
        cycle: Uuid,
    ) -> Result<Option<DistributionManifest>, AppError> {
        let mut tx = self.transaction(scope).await?;
        let row = sqlx::query(
            "SELECT frozen,expansion_cursor,complete,sealed_at FROM distribution_execution_manifests \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND cycle_id=$4 \
             ORDER BY revision DESC LIMIT 1"
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .bind(cycle).fetch_optional(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        row.as_ref().map(from_row).transpose()
    }

    async fn as_of(
        &self,
        scope: &TenantScope,
        id: Uuid,
        at: DateTime<Utc>,
    ) -> Result<DistributionSnapshot, AppError> {
        let mut tx = self.transaction(scope).await?;
        let row = sqlx::query(
            "SELECT frozen,expansion_cursor,complete,sealed_at FROM distribution_execution_manifests \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND manifest_id=$4 AND sealed_at<=$5"
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .bind(id).bind(at).fetch_optional(&mut *tx).await.map_err(db)?
            .ok_or_else(|| AppError::not_found("manifest was not frozen at cutoff"))?;
        let manifest = from_row(&row)?;
        let versions = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT DISTINCT ON (v.target_id) v.body FROM distribution_target_versions v \
             JOIN distribution_execution_targets t ON t.target_id=v.target_id \
               AND t.operator_id=v.operator_id AND t.tenant_id=v.tenant_id AND t.project_id=v.project_id \
             WHERE v.operator_id=$1 AND v.tenant_id=$2 AND v.project_id=$3 \
               AND t.manifest_id=$4 AND v.recorded_at<=$5 \
             ORDER BY v.target_id,v.version DESC"
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .bind(id).bind(at).fetch_all(&mut *tx).await.map_err(db)?;
        let mut targets: Vec<DistributionTarget> =
            versions.into_iter().map(decode).collect::<Result<_, _>>()?;
        targets.sort_by_key(|target| target.ordinal);
        tx.commit().await.map_err(db)?;
        Ok(DistributionSnapshot {
            manifest,
            targets,
            as_of: at,
        })
    }

    async fn expansion_page(
        &self,
        scope: &TenantScope,
        id: Uuid,
        cursor: u64,
        limit: usize,
    ) -> Result<DistributionExpansionPage, AppError> {
        let manifest = self.get(scope, id).await?;
        if limit == 0 || cursor > manifest.expected_count {
            return Err(AppError::invalid_request(
                "invalid expansion cursor or page limit",
            ));
        }
        let end = cursor
            .saturating_add(limit as u64)
            .min(manifest.expected_count);
        Ok(DistributionExpansionPage {
            manifest_id: id,
            cursor,
            next_cursor: end,
            rows: (cursor..end)
                .map(|ordinal| distribution_cell(&manifest, ordinal))
                .collect(),
        })
    }

    async fn commit_expansion_page(
        &self,
        scope: &TenantScope,
        id: Uuid,
        expected_cursor: u64,
        rows: Vec<DistributionTarget>,
    ) -> Result<DistributionManifest, AppError> {
        if rows.is_empty() {
            return Err(AppError::invalid_request("expansion page is empty"));
        }
        let mut tx = self.transaction(scope).await?;
        let mut manifest = locked_manifest(&mut tx, scope, id).await?;
        let end = expected_cursor
            .checked_add(rows.len() as u64)
            .ok_or_else(|| AppError::invalid_request("expansion cursor overflow"))?;
        if end > manifest.expected_count
            || rows.iter().enumerate().any(|(offset, row)| {
                row != &distribution_cell(&manifest, expected_cursor + offset as u64)
            })
        {
            return Err(AppError::conflict(
                "expansion rows do not match frozen roster",
            ));
        }
        if manifest.expansion_cursor != expected_cursor {
            if end <= manifest.expansion_cursor {
                for row in &rows {
                    let first: Option<serde_json::Value> = sqlx::query_scalar(
                        "SELECT body FROM distribution_target_versions WHERE operator_id=$1 \
                         AND tenant_id=$2 AND project_id=$3 AND target_id=$4 AND version=1",
                    )
                    .bind(scope.operator_id.as_uuid())
                    .bind(scope.tenant_id.as_uuid())
                    .bind(project(scope)?)
                    .bind(row.target_id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(db)?;
                    if first.map(decode::<DistributionTarget>).transpose()? != Some(row.clone()) {
                        return Err(AppError::conflict("expansion page replay differs"));
                    }
                }
                tx.commit().await.map_err(db)?;
                return Ok(manifest);
            }
            return Err(AppError::conflict("expansion cursor changed"));
        }
        for row in rows {
            let body = encode(&row)?;
            sqlx::query(
                "INSERT INTO distribution_execution_targets \
                 (target_id,operator_id,tenant_id,project_id,manifest_id,ordinal,current_version,current_body) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8)"
            ).bind(row.target_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
                .bind(project(scope)?).bind(id).bind(row.ordinal as i64)
                .bind(row.version as i64).bind(body.clone()).execute(&mut *tx).await.map_err(db)?;
            sqlx::query(
                "INSERT INTO distribution_target_versions \
                 (target_id,operator_id,tenant_id,project_id,version,body) VALUES ($1,$2,$3,$4,$5,$6)"
            ).bind(row.target_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
                .bind(project(scope)?).bind(row.version as i64).bind(body).execute(&mut *tx).await.map_err(db)?;
        }
        sqlx::query(
            "UPDATE distribution_execution_manifests SET expansion_cursor=$1,complete=$2 \
             WHERE operator_id=$3 AND tenant_id=$4 AND project_id=$5 AND manifest_id=$6 AND expansion_cursor=$7"
        ).bind(end as i64).bind(end == manifest.expected_count).bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid()).bind(project(scope)?).bind(id)
            .bind(expected_cursor as i64).execute(&mut *tx).await.map_err(db)?;
        manifest.expansion_cursor = end;
        manifest.complete = end == manifest.expected_count;
        tx.commit().await.map_err(db)?;
        Ok(manifest)
    }

    async fn materialize(
        &self,
        scope: &TenantScope,
        prepared: PreparedDistribution,
    ) -> Result<MaterializedDistribution, AppError> {
        let mut tx = self.transaction(scope).await?;
        // Project-scoped serialization is necessary because two different
        // cycle manifests can race to create the same logical publication.
        let locked: Option<String> = sqlx::query_scalar(
            "SELECT status FROM projects WHERE operator_id=$1 AND tenant_id=$2 \
             AND project_id=$3 FOR UPDATE",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        let Some(project_status) = locked else {
            return Err(AppError::not_found("project not found"));
        };
        let manifest = locked_manifest(&mut tx, scope, prepared.manifest_id).await?;
        if !manifest.complete {
            return Err(AppError::not_ready("distribution expansion incomplete"));
        }
        let old = current_target(&mut tx, scope, prepared.target_id).await?;
        if old.manifest_id != manifest.manifest_id {
            return Err(AppError::not_found("target belongs to another manifest"));
        }
        if !matches!(
            old.status,
            DistributionTargetStatus::Pending
                | DistributionTargetStatus::Deferred
                | DistributionTargetStatus::Ready
                | DistributionTargetStatus::ReusedUnknown
                | DistributionTargetStatus::ReusedVerified
        ) {
            return Err(AppError::conflict("target is not publishable"));
        }
        if let Some(reason) = prepared.defer_reason {
            let mut next = old.clone();
            next.status = DistributionTargetStatus::Deferred;
            next.reason = Some(reason.code().into());
            let next = change_target(&mut tx, scope, &old, next).await?;
            let variant = if let Some(id) = old.variant_id {
                sqlx::query_scalar::<_, serde_json::Value>(
                    "SELECT body FROM distribution_channel_variants WHERE operator_id=$1 AND tenant_id=$2 \
                     AND project_id=$3 AND variant_id=$4"
                ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
                    .bind(project(scope)?).bind(id).fetch_optional(&mut *tx).await.map_err(db)?
                    .map(decode::<ChannelVariant>).transpose()?
            } else {
                None
            };
            let intent = if let Some(id) = old.publication_intent_id {
                let row = sqlx::query(
                    "SELECT body,verification,verification_evidence_id FROM distribution_publication_intents \
                     WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND intent_id=$4"
                ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
                    .bind(project(scope)?).bind(id).fetch_optional(&mut *tx).await.map_err(db)?;
                row.map(|row| {
                    let mut intent: PublicationIntent = decode(row.get("body"))?;
                    intent.verification =
                        decode(serde_json::Value::String(row.get("verification")))?;
                    intent.verification_evidence_id = row.get("verification_evidence_id");
                    Ok::<_, AppError>(intent)
                })
                .transpose()?
            } else {
                None
            };
            tx.commit().await.map_err(db)?;
            return Ok(MaterializedDistribution {
                target: next,
                variant,
                intent,
                publication_commands: vec![],
            });
        }
        if old.status == DistributionTargetStatus::Deferred
            && !matches!(
                old.reason.as_deref(),
                Some(
                    "account_unassigned"
                        | "source_unavailable"
                        | "source_changed"
                        | "content_unsupported"
                )
            )
        {
            return Err(AppError::conflict(
                "target is deferred by capability or content",
            ));
        }
        let placement = manifest
            .platform_scope
            .iter()
            .find(|p| p.platform_id == old.platform_id && p.placement_slot == old.placement_slot)
            .ok_or_else(|| AppError::new(ErrorCode::Internal, "frozen placement missing"))?;
        let mut next = old.clone();
        let Some(account_id) = prepared.account_id else {
            if old.publication_intent_id.is_some() {
                return Err(AppError::conflict("assigned intent cannot be unassigned"));
            }
            next.status = DistributionTargetStatus::Deferred;
            next.reason = Some("account_unassigned".into());
            let next = change_target(&mut tx, scope, &old, next).await?;
            tx.commit().await.map_err(db)?;
            return Ok(MaterializedDistribution {
                target: next,
                variant: None,
                intent: None,
                publication_commands: vec![],
            });
        };
        let revision = prepared
            .revision
            .as_ref()
            .ok_or_else(|| AppError::invalid_request("content revision required"))?;
        if Some(revision.revision_id) != old.content_revision_id {
            return Err(AppError::conflict(
                "content revision differs from frozen handoff",
            ));
        }
        let stored_revision: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT body FROM content_revisions WHERE operator_id=$1 AND tenant_id=$2 \
             AND project_id=$3 AND revision_id=$4",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(revision.revision_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        let canonical: ContentRevision = decode(
            stored_revision.ok_or_else(|| AppError::not_found("content revision not found"))?,
        )?;
        let mut checked_canonical = canonical;
        checked_canonical.findings = revision.findings.clone();
        if checked_canonical != *revision {
            return Err(AppError::conflict(
                "content revision differs from immutable storage",
            ));
        }
        let stored_check: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT body FROM content_checks WHERE operator_id=$1 AND tenant_id=$2 \
             AND project_id=$3 AND revision_id=$4",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(revision.revision_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        let check: ContentCheck = decode(
            stored_check
                .ok_or_else(|| AppError::conflict("independent content check is missing"))?,
        )?;
        if check.revision_id != revision.revision_id
            || check.findings.iter().any(|finding| finding.blocking)
            || revision.findings.iter().any(|finding| finding.blocking)
        {
            return Err(AppError::conflict(
                "content revision has no successful independent check",
            ));
        }
        let ineligible = if project_status != "active" {
            Some(DistributionDeferralReason::SourceUnavailable)
        } else {
            live_materialization_sources(&mut tx, scope, &manifest, &old, revision).await?
        };
        if let Some(reason) = ineligible {
            if old.publication_intent_id.is_some() {
                // A prior unknown/sent intent remains bound to its original
                // reconciliation path. Revoke must never create a new send.
                return Err(AppError::conflict(
                    "bound publication source is no longer eligible",
                ));
            }
            let mut next = old.clone();
            next.status = DistributionTargetStatus::Deferred;
            next.reason = Some(reason.code().into());
            let next = change_target(&mut tx, scope, &old, next).await?;
            tx.commit().await.map_err(db)?;
            return Ok(MaterializedDistribution {
                target: next,
                variant: None,
                intent: None,
                publication_commands: vec![],
            });
        }
        let variant = prepare_variant(revision, placement)?;
        if let Some(stored) = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT body FROM distribution_channel_variants WHERE operator_id=$1 AND tenant_id=$2 \
             AND project_id=$3 AND variant_id=$4",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(variant.variant_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        {
            let previous: ChannelVariant = decode(stored)?;
            if previous != variant {
                return Err(AppError::conflict("variant identity differs"));
            }
        } else {
            sqlx::query(
                "INSERT INTO distribution_channel_variants \
                 (variant_id,operator_id,tenant_id,project_id,content_revision_id,body) \
                 VALUES ($1,$2,$3,$4,$5,$6)",
            )
            .bind(variant.variant_id)
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project(scope)?)
            .bind(revision.revision_id)
            .bind(encode(&variant)?)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        }
        let (candidate, command) =
            prepare_publication_intent(scope, &manifest, &old, &variant, account_id, Utc::now());
        let stored = sqlx::query(
            "SELECT body,verification,verification_evidence_id FROM distribution_publication_intents \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND logical_key=$4 FOR UPDATE"
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
            .bind(project(scope)?).bind(&candidate.logical_key).fetch_optional(&mut *tx).await.map_err(db)?;
        let previous = if let Some(row) = stored {
            let mut old_intent: PublicationIntent = decode(row.get("body"))?;
            old_intent.verification = decode(serde_json::Value::String(row.get("verification")))?;
            old_intent.verification_evidence_id = row.get("verification_evidence_id");
            if old_intent.intent_id != candidate.intent_id
                || old_intent.variant_id != variant.variant_id
                || old_intent.content_revision_id != revision.revision_id
                || old_intent.account_id != account_id
                || old_intent.payload_hash != variant.payload_hash
            {
                return Err(AppError::conflict(
                    "publication intent logical identity differs",
                ));
            }
            Some(old_intent)
        } else {
            None
        };
        if old
            .publication_intent_id
            .is_some_and(|id| id != candidate.intent_id)
            || old.account_id.is_some_and(|id| id != account_id)
            || old.variant_id.is_some_and(|id| id != variant.variant_id)
        {
            return Err(AppError::conflict(
                "target is already bound to another intent",
            ));
        }
        let intent = previous.clone().unwrap_or(candidate);
        let commands = if previous.is_none() {
            sqlx::query(
                "INSERT INTO distribution_publication_intents \
                 (intent_id,operator_id,tenant_id,project_id,origin_target_id,variant_id,logical_key,body) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8)"
            ).bind(intent.intent_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
                .bind(project(scope)?).bind(old.target_id).bind(variant.variant_id)
                .bind(&intent.logical_key).bind(encode(&intent)?).execute(&mut *tx).await.map_err(db)?;
            sqlx::query(
                "INSERT INTO distribution_publication_commands \
                 (command_id,operator_id,tenant_id,project_id,intent_id,origin_target_id,payload_hash,fixture) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8)"
            ).bind(command.command_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
                .bind(project(scope)?).bind(intent.intent_id).bind(old.target_id)
                .bind(&command.payload_hash).bind(command.fixture).execute(&mut *tx).await.map_err(db)?;
            vec![command]
        } else {
            vec![]
        };
        next.variant_id = Some(variant.variant_id);
        next.account_id = Some(account_id);
        next.publication_intent_id = Some(intent.intent_id);
        next.status = match &previous {
            Some(prior) if prior.verification == IntentVerification::Verified => {
                DistributionTargetStatus::ReusedVerified
            }
            Some(prior) if prior.verification == IntentVerification::Unknown => {
                DistributionTargetStatus::ReusedUnknown
            }
            Some(prior)
                if prior.channel_target_id == old.target_id
                    && old.status == DistributionTargetStatus::Deferred =>
            {
                DistributionTargetStatus::Ready
            }
            Some(prior) if prior.channel_target_id == old.target_id => old.status,
            Some(_) => DistributionTargetStatus::ReusedUnknown,
            None => DistributionTargetStatus::Ready,
        };
        next.reason = None;
        let next = change_target(&mut tx, scope, &old, next).await?;
        tx.commit().await.map_err(db)?;
        Ok(MaterializedDistribution {
            target: next,
            variant: Some(variant),
            intent: Some(intent),
            publication_commands: commands,
        })
    }

    async fn record_intent_verification(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
        verification: IntentVerification,
        evidence_id: Uuid,
    ) -> Result<PublicationIntent, AppError> {
        if evidence_id.is_nil() || verification == IntentVerification::Unverified {
            return Err(AppError::invalid_request(
                "receipt evidence and a resolved verification state are required",
            ));
        }
        let mut tx = self.transaction(scope).await?;
        let row = sqlx::query(
            "SELECT body,verification,verification_evidence_id FROM distribution_publication_intents \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND intent_id=$4 FOR UPDATE"
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .bind(intent_id).fetch_optional(&mut *tx).await.map_err(db)?
            .ok_or_else(|| AppError::not_found("publication intent not found"))?;
        let mut intent: PublicationIntent = decode(row.get("body"))?;
        intent.verification = decode(serde_json::Value::String(row.get("verification")))?;
        intent.verification_evidence_id = row.get("verification_evidence_id");
        if intent.verification == IntentVerification::Verified
            && verification == IntentVerification::Unknown
        {
            return Err(AppError::conflict(
                "verified asset cannot revert to unknown",
            ));
        }
        let result: Option<String> = sqlx::query_scalar(
            "SELECT e.result FROM distribution_intent_evidence e \
             JOIN distribution_publication_attempts a ON a.operator_id=e.operator_id \
               AND a.tenant_id=e.tenant_id AND a.project_id=e.project_id \
               AND a.intent_id=e.intent_id AND a.attempt_id=e.attempt_id \
             JOIN distribution_publication_commands c ON c.operator_id=a.operator_id \
               AND c.tenant_id=a.tenant_id AND c.project_id=a.project_id \
               AND c.intent_id=a.intent_id AND c.command_id=a.command_id \
             WHERE e.operator_id=$1 AND e.tenant_id=$2 AND e.project_id=$3 \
               AND e.intent_id=$4 AND e.evidence_id=$5 \
               AND c.status IN ('claimed','delivered') \
               AND (e.result='unknown' OR (c.fixture=false AND e.fixture=false \
                 AND e.external_receipt IS NOT NULL AND e.public_readback IS NOT NULL))",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(intent_id)
        .bind(evidence_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        let desired = match verification {
            IntentVerification::Unknown => "unknown",
            IntentVerification::Verified => "verified",
            IntentVerification::Unverified => unreachable!(),
        };
        if result.as_deref() != Some(desired) {
            return Err(AppError::conflict(
                "persisted receipt evidence is required for verification",
            ));
        }
        intent.verification = verification;
        intent.verification_evidence_id = Some(evidence_id);
        sqlx::query(
            "UPDATE distribution_publication_intents SET verification=$1,verification_evidence_id=$2,body=$3 \
             WHERE operator_id=$4 AND tenant_id=$5 AND project_id=$6 AND intent_id=$7"
        ).bind(desired).bind(evidence_id).bind(encode(&intent)?)
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .bind(intent_id).execute(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(intent)
    }

    async fn list_targets(
        &self,
        scope: &TenantScope,
        id: Uuid,
        after_ordinal: Option<u64>,
        limit: usize,
    ) -> Result<DistributionTargetPage, AppError> {
        if limit == 0 || limit > 1000 {
            return Err(AppError::invalid_request("invalid target page size"));
        }
        let manifest = self.get(scope, id).await?;
        let mut tx = self.transaction(scope).await?;
        let rows: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT current_body FROM distribution_execution_targets WHERE operator_id=$1 AND tenant_id=$2 \
             AND project_id=$3 AND manifest_id=$4 AND ($5::BIGINT IS NULL OR ordinal>$5) \
             ORDER BY ordinal LIMIT $6"
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .bind(id).bind(after_ordinal.map(|n| n as i64)).bind(limit as i64)
            .fetch_all(&mut *tx).await.map_err(db)?;
        let targets: Vec<DistributionTarget> =
            rows.into_iter().map(decode).collect::<Result<_, _>>()?;
        let next_ordinal = targets
            .last()
            .map(|target| target.ordinal)
            .filter(|ordinal| *ordinal + 1 < manifest.expansion_cursor);
        tx.commit().await.map_err(db)?;
        Ok(DistributionTargetPage {
            manifest_id: id,
            rows: targets,
            next_ordinal,
            expected_count: manifest.expected_count,
        })
    }

    async fn cycle_inputs(
        &self,
        scope: &TenantScope,
        cycle: Uuid,
        at: DateTime<Utc>,
    ) -> Result<DistributionCycleInputs, AppError> {
        let mut tx = self.transaction(scope).await?;
        let row = sqlx::query(
            "SELECT frozen,expansion_cursor,complete,sealed_at FROM distribution_execution_manifests \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND cycle_id=$4 AND sealed_at<=$5 \
             ORDER BY revision DESC LIMIT 1"
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .bind(cycle).bind(at).fetch_optional(&mut *tx).await.map_err(db)?;
        let manifest = row.as_ref().map(from_row).transpose()?;
        tx.commit().await.map_err(db)?;
        match manifest {
            Some(manifest) => {
                let snapshot = self.as_of(scope, manifest.manifest_id, at).await?;
                let temporally_complete = snapshot.targets.len() as u64 == manifest.expected_count;
                Ok(DistributionCycleInputs {
                    manifest: Some(manifest),
                    targets: snapshot.targets,
                    temporally_complete,
                })
            }
            None => Ok(DistributionCycleInputs {
                manifest: None,
                targets: vec![],
                temporally_complete: false,
            }),
        }
    }
}
