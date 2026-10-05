use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use geo_domain::{
    AppError, ChannelOutcome, ChannelOutcomeStatus, ChannelTarget, ChannelTargetInput, ErrorCode,
    ProjectId, PublicationLookupCandidate, PublicationLookupFinding, PublicationLookupJob,
    PublicationLookupObservation, PublicationLookupRepository, TenantScope,
};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

#[derive(Clone)]
pub struct PgPublicationLookupRepository {
    pool: PgPool,
}

/// A trusted, scoped original send that still needs a lookup job or one-time
/// enrichment from its finalized Unknown outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationLookupDiscoveryCandidate {
    pub scope: TenantScope,
    pub target_id: Uuid,
    pub attempt_id: Uuid,
}

impl PgPublicationLookupRepository {
    pub fn from_database(database: &crate::Database) -> Self {
        Self {
            pool: database.pool().clone(),
        }
    }

    /// Discover original publication sends, never measurements or fresh
    /// in-flight sends. An early job is rediscovered only when a later Unknown
    /// result can enrich its initially absent connector version/hint; a job
    /// already synchronized with that immutable result is not scanned again.
    /// The original send may need reconciliation even if its project is paused.
    pub async fn scan_unresolved(
        &self,
        after_attempt_id: Option<Uuid>,
        as_of: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<PublicationLookupDiscoveryCandidate>, AppError> {
        if limit == 0 || limit > 1000 {
            return Err(AppError::invalid_request("invalid lookup scan page size"));
        }
        let rows = sqlx::query(
            "SELECT a.operator_id,a.tenant_id,a.project_id,a.target_id,a.attempt_id \
             FROM channel_execution_attempts a \
             JOIN channel_execution_targets t ON t.operator_id=a.operator_id \
               AND t.tenant_id=a.tenant_id AND t.project_id=a.project_id AND t.target_id=a.target_id \
             LEFT JOIN publication_lookup_jobs j ON j.operator_id=a.operator_id \
               AND j.tenant_id=a.tenant_id AND j.project_id=a.project_id AND j.attempt_id=a.attempt_id \
             WHERE ($1::uuid IS NULL OR a.attempt_id>$1) \
               AND a.target_kind='publish' AND t.kind='publish' \
               AND a.claimed_at + interval '5 minutes' <= $2 \
               AND (a.outcome IS NULL OR a.outcome->>'status'='unknown') \
               AND (j.attempt_id IS NULL OR \
                    (j.connector_version IS NULL AND a.outcome->>'status'='unknown' \
                     AND NULLIF(a.outcome->>'connector_version','') IS NOT NULL)) \
             ORDER BY a.attempt_id LIMIT $3",
        )
        .bind(after_attempt_id)
        .bind(as_of)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        Ok(rows
            .iter()
            .map(|row| PublicationLookupDiscoveryCandidate {
                scope: TenantScope::new(
                    row.get::<Uuid, _>("operator_id").into(),
                    row.get::<Uuid, _>("tenant_id").into(),
                    Some(ProjectId::new(row.get("project_id"))),
                ),
                target_id: row.get("target_id"),
                attempt_id: row.get("attempt_id"),
            })
            .collect())
    }
}

fn db(error: sqlx::Error) -> AppError {
    if error
        .as_database_error()
        .is_some_and(|db| db.is_unique_violation())
    {
        AppError::conflict("lookup execution already exists")
    } else {
        AppError::new(
            ErrorCode::DependencyUnavailable,
            "publication lookup unavailable",
        )
    }
}

fn project(scope: &TenantScope) -> Result<Uuid, AppError> {
    scope
        .project_id
        .map(|id| id.as_uuid())
        .ok_or_else(|| AppError::forbidden("project scope required"))
}

const JOB_FIELDS: &str = "attempt_id,target_id,account_id,frozen_input,connector_version,\
    candidate_public_url,next_due_at,lease_execution_id,lease_expires_at,query_count,last_error_code";

fn job(row: &sqlx::postgres::PgRow) -> Result<PublicationLookupJob, AppError> {
    let json: serde_json::Value = row.get("frozen_input");
    let frozen_input: ChannelTargetInput = serde_json::from_value(json)
        .map_err(|_| AppError::new(ErrorCode::Internal, "invalid frozen lookup target"))?;
    Ok(PublicationLookupJob {
        attempt_id: row.get("attempt_id"),
        target_id: row.get("target_id"),
        account_id: row.get("account_id"),
        frozen_input,
        connector_version: row.get("connector_version"),
        candidate_public_url: row.get("candidate_public_url"),
        next_due_at: row.get("next_due_at"),
        lease_execution_id: row.get("lease_execution_id"),
        lease_expires_at: row.get("lease_expires_at"),
        query_count: row.get("query_count"),
        last_error_code: row.get("last_error_code"),
    })
}

fn public_candidate(url: &str, platform: &str) -> bool {
    // Currently only the fixed adapter public asset route is supported. No
    // query/fragment/userinfo/redirect/other host can become a lookup hint.
    platform == "zhihu"
        && ["https://www.zhihu.com/p/", "https://zhuanlan.zhihu.com/p/"]
            .iter()
            .any(|prefix| {
                url.strip_prefix(prefix)
                    .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
            })
}

fn candidate(
    outcome: Option<&ChannelOutcome>,
    input: &ChannelTargetInput,
    attempt_id: Uuid,
    target_id: Uuid,
    account_id: Uuid,
    claimed_at: DateTime<Utc>,
    received_at: Option<DateTime<Utc>>,
) -> Option<String> {
    let (platform, title, body) = match input {
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
        } => (platform.as_str(), title.as_str(), body.as_str()),
        ChannelTargetInput::Measure { .. } => return None,
    };
    let outcome = outcome?;
    if outcome.fixture || outcome.status != ChannelOutcomeStatus::Unknown {
        return None;
    }
    let version = outcome.connector_version.as_deref()?;
    let normalize = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let expected = hex::encode(Sha256::digest(
        format!("{}\n{}", normalize(title), normalize(body)).as_bytes(),
    ));
    let mut matches = outcome.runner_evidence.iter().filter_map(|evidence| {
        (evidence.get("kind")?.as_str()? == "publication_candidate"
            && evidence.get("schema_version")?.as_str()? == "geo.publication.candidate.v1"
            && evidence.get("source")?.as_str()? == "post_submit_navigation"
            && evidence.get("attempt_id")?.as_str()? == attempt_id.to_string()
            && evidence.get("target_id")?.as_str()? == target_id.to_string()
            && evidence.get("account_id")?.as_str()? == account_id.to_string()
            && evidence.get("connector_version")?.as_str()? == version
            && evidence.get("expected_sha256")?.as_str()? == expected
            && evidence
                .get("observed_at")?
                .as_str()?
                .parse::<DateTime<Utc>>()
                .ok()
                .is_some_and(|observed_at| {
                    observed_at >= claimed_at
                        && received_at.is_some_and(|received_at| {
                            // PostgreSQL stores microseconds; the original
                            // JSON retains sub-microsecond precision.
                            observed_at <= received_at + Duration::microseconds(1)
                        })
                }))
        .then(|| evidence.get("url")?.as_str())
        .flatten()
        .filter(|url| public_candidate(url, platform))
        .map(str::to_string)
    });
    let first = matches.next()?;
    // Multiple conflicting hints are ambiguous, never silently select one.
    matches.all(|other| other == first).then_some(first)
}

fn finding_name(finding: PublicationLookupFinding) -> &'static str {
    match finding {
        PublicationLookupFinding::Unknown => "unknown",
        PublicationLookupFinding::AssetObserved => "asset_observed",
    }
}

fn pg_time(time: DateTime<Utc>) -> DateTime<Utc> {
    // PostgreSQL TIMESTAMPTZ stores microseconds, whereas Chrono may contain
    // nanoseconds. Normalize before persistence and replay comparisons.
    DateTime::from_timestamp_micros(time.timestamp_micros()).expect("valid timestamp")
}

fn decode_observation(
    row: &sqlx::postgres::PgRow,
) -> Result<PublicationLookupObservation, AppError> {
    let finding = match row.get::<&str, _>("finding") {
        "unknown" => PublicationLookupFinding::Unknown,
        "asset_observed" => PublicationLookupFinding::AssetObserved,
        _ => return Err(AppError::new(ErrorCode::Internal, "invalid lookup finding")),
    };
    Ok(PublicationLookupObservation {
        execution_id: row.get("execution_id"),
        attempt_id: row.get("attempt_id"),
        finding,
        evidence: row.get("evidence"),
        observed_at: row.get("observed_at"),
        received_at: row.get("received_at"),
        error_code: row.get("error_code"),
    })
}

async fn get_locked(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    attempt_id: Uuid,
) -> Result<PublicationLookupJob, AppError> {
    let query = format!(
        "SELECT {JOB_FIELDS} FROM publication_lookup_jobs \
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND attempt_id=$4 FOR UPDATE"
    );
    let row = sqlx::query(&query)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(attempt_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db)?
        .ok_or_else(|| AppError::not_found("lookup job not found"))?;
    job(&row)
}

#[async_trait]
impl PublicationLookupRepository for PgPublicationLookupRepository {
    async fn enqueue(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<PublicationLookupJob, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let row = sqlx::query(
            "SELECT a.account_id,a.claimed_at,a.received_at,a.outcome,t.frozen_input FROM channel_execution_attempts a \
             JOIN channel_execution_targets t ON t.operator_id=a.operator_id \
              AND t.tenant_id=a.tenant_id AND t.project_id=a.project_id AND t.target_id=a.target_id \
             WHERE a.operator_id=$1 AND a.tenant_id=$2 AND a.project_id=$3 \
              AND a.attempt_id=$4 AND a.target_id=$5 \
              AND a.target_kind='publish' AND t.kind='publish' FOR UPDATE OF a",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(attempt_id)
        .bind(target_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or_else(|| AppError::not_found("ambiguous publication attempt not found"))?;
        let account_id: Uuid = row.get("account_id");
        let target: ChannelTarget = serde_json::from_value(row.get("frozen_input"))
            .map_err(|_| AppError::new(ErrorCode::Internal, "invalid frozen channel target"))?;
        if target.target_id != target_id
            || !target.input.is_publication()
            || target.input.account_id() != account_id
        {
            return Err(AppError::conflict("publication target binding differs"));
        }
        let outcome: Option<ChannelOutcome> = row
            .get::<Option<serde_json::Value>, _>("outcome")
            .map(|value| {
                serde_json::from_value(value)
                    .map_err(|_| AppError::new(ErrorCode::Internal, "invalid send outcome"))
            })
            .transpose()?;
        if outcome
            .as_ref()
            .is_some_and(|value| value.status != ChannelOutcomeStatus::Unknown)
        {
            return Err(AppError::conflict("publication send is not ambiguous"));
        }
        let hint = candidate(
            outcome.as_ref(),
            &target.input,
            attempt_id,
            target_id,
            account_id,
            row.get("claimed_at"),
            row.get("received_at"),
        );
        let version = outcome
            .as_ref()
            .and_then(|value| value.connector_version.clone());
        let frozen_input = serde_json::to_value(&target.input)
            .map_err(|_| AppError::new(ErrorCode::Internal, "lookup input encoding failed"))?;
        sqlx::query(
            "INSERT INTO publication_lookup_jobs \
              (attempt_id,operator_id,tenant_id,project_id,target_id,account_id,frozen_input,\
               connector_version,candidate_public_url,next_due_at,created_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$10) ON CONFLICT (attempt_id) DO NOTHING",
        )
        .bind(attempt_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(target_id)
        .bind(account_id)
        .bind(frozen_input)
        .bind(&version)
        .bind(&hint)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        let mut stored = get_locked(&mut tx, scope, attempt_id).await?;
        // The send can finish Unknown after an early crash-recovery enqueue.
        // Enrich only its originally empty hint/version from the same scoped,
        // now-finalized attempt; never replace an existing hint or version.
        if outcome.is_some()
            && version.is_some()
            && stored.connector_version.is_none()
            && stored.candidate_public_url.is_none()
            && stored.target_id == target_id
            && stored.account_id == account_id
            && stored.frozen_input == target.input
        {
            sqlx::query(
                "UPDATE publication_lookup_jobs SET connector_version=$1,candidate_public_url=$2, \
                   next_due_at=CASE WHEN $2::text IS NOT NULL THEN COALESCE(next_due_at,$7) \
                                    ELSE next_due_at END \
                 WHERE operator_id=$3 AND tenant_id=$4 AND project_id=$5 AND attempt_id=$6 \
                   AND connector_version IS NULL AND candidate_public_url IS NULL",
            )
            .bind(&version)
            .bind(&hint)
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project(scope)?)
            .bind(attempt_id)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
            stored = get_locked(&mut tx, scope, attempt_id).await?;
        }
        if stored.target_id != target_id
            || stored.account_id != account_id
            || stored.frozen_input != target.input
            || stored.connector_version != version
            || stored.candidate_public_url != hint
        {
            return Err(AppError::conflict("lookup job snapshot differs"));
        }
        tx.commit().await.map_err(db)?;
        Ok(stored)
    }

    async fn scan_due(
        &self,
        after_attempt_id: Option<Uuid>,
        as_of: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<PublicationLookupCandidate>, AppError> {
        if limit == 0 || limit > 1000 {
            return Err(AppError::invalid_request("invalid lookup scan page size"));
        }
        let rows = sqlx::query(
            "SELECT operator_id,tenant_id,project_id,attempt_id FROM publication_lookup_jobs \
             WHERE ($1::uuid IS NULL OR attempt_id>$1) \
               AND next_due_at<=$2 AND (lease_expires_at IS NULL OR lease_expires_at<=$2) \
             ORDER BY attempt_id LIMIT $3",
        )
        .bind(after_attempt_id)
        .bind(as_of)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        Ok(rows
            .iter()
            .map(|row| PublicationLookupCandidate {
                scope: TenantScope::new(
                    row.get::<Uuid, _>("operator_id").into(),
                    row.get::<Uuid, _>("tenant_id").into(),
                    Some(ProjectId::new(row.get("project_id"))),
                ),
                attempt_id: row.get("attempt_id"),
            })
            .collect())
    }

    async fn claim(
        &self,
        scope: &TenantScope,
        attempt_id: Uuid,
        execution_id: Uuid,
        at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<PublicationLookupJob, AppError> {
        if expires_at <= at || expires_at - at > Duration::minutes(5) {
            return Err(AppError::invalid_request("invalid lookup lease duration"));
        }
        let query = format!(
            "UPDATE publication_lookup_jobs SET lease_execution_id=$5,lease_expires_at=$6,\
             query_count=query_count+1 WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 \
             AND attempt_id=$4 AND next_due_at<=$7 \
             AND $6>clock_timestamp() AND $6<=clock_timestamp()+interval '5 minutes' \
             AND (lease_expires_at IS NULL OR lease_expires_at<=clock_timestamp()) \
             RETURNING {JOB_FIELDS}"
        );
        let mut tx = self.pool.begin().await.map_err(db)?;
        let current = get_locked(&mut tx, scope, attempt_id).await?;
        if !current.next_due_at.is_some_and(|due| due <= at)
            || current.lease_expires_at.is_some_and(|expiry| expiry > at)
        {
            return Err(AppError::conflict("lookup job not due or already leased"));
        }
        sqlx::query(
            "INSERT INTO publication_lookup_executions \
             (execution_id,operator_id,tenant_id,project_id,attempt_id,claimed_at,expires_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(execution_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(attempt_id)
        .bind(at)
        .bind(expires_at)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        let row = sqlx::query(&query)
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project(scope)?)
            .bind(attempt_id)
            .bind(execution_id)
            .bind(expires_at)
            .bind(at)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db)?
            .ok_or_else(|| AppError::conflict("lookup job not due or already leased"))?;
        let claimed = job(&row)?;
        tx.commit().await.map_err(db)?;
        Ok(claimed)
    }

    async fn finish(
        &self,
        scope: &TenantScope,
        attempt_id: Uuid,
        observation: PublicationLookupObservation,
        next_due_at: Option<DateTime<Utc>>,
    ) -> Result<PublicationLookupJob, AppError> {
        let observation = PublicationLookupObservation {
            observed_at: pg_time(observation.observed_at),
            received_at: pg_time(observation.received_at),
            ..observation
        };
        let next_due_at = next_due_at.map(pg_time);
        if observation.attempt_id != attempt_id
            || observation.received_at < observation.observed_at
            || next_due_at.is_some_and(|at| at <= observation.received_at)
            || observation.error_code.as_deref().is_some_and(|code| {
                code.is_empty()
                    || code.len() > 64
                    || !code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
            })
        {
            return Err(AppError::invalid_request("invalid lookup observation"));
        }
        let mut tx = self.pool.begin().await.map_err(db)?;
        let current = get_locked(&mut tx, scope, attempt_id).await?;
        let previous = sqlx::query(
            "SELECT execution_id,attempt_id,finding,evidence,observed_at,received_at,error_code,\
             next_due_at FROM publication_lookup_observations WHERE operator_id=$1 AND tenant_id=$2 \
             AND project_id=$3 AND execution_id=$4",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(observation.execution_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        if let Some(previous) = previous {
            if decode_observation(&previous)? != observation
                || previous.get::<Option<DateTime<Utc>>, _>("next_due_at") != next_due_at
            {
                return Err(AppError::conflict("lookup execution differs"));
            }
            tx.commit().await.map_err(db)?;
            return Ok(current);
        }
        let lease_alive: bool = sqlx::query_scalar(
            "SELECT COALESCE(lease_expires_at>clock_timestamp(),false) FROM publication_lookup_jobs \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND attempt_id=$4",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(attempt_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        if current.lease_execution_id != Some(observation.execution_id)
            || !current
                .lease_expires_at
                .is_some_and(|expires| expires > observation.received_at)
            || !lease_alive
        {
            return Err(AppError::conflict("lookup lease expired or replaced"));
        }
        let claimed_at: DateTime<Utc> = sqlx::query_scalar(
            "SELECT claimed_at FROM publication_lookup_executions \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND execution_id=$4 \
               AND attempt_id=$5",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(observation.execution_id)
        .bind(attempt_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or_else(|| AppError::conflict("lookup execution missing"))?;
        if observation.observed_at < claimed_at
            || observation.received_at < claimed_at
            || observation.observed_at > observation.received_at
        {
            return Err(AppError::invalid_request(
                "lookup observation predates claim",
            ));
        }
        sqlx::query(
            "INSERT INTO publication_lookup_observations \
             (execution_id,operator_id,tenant_id,project_id,attempt_id,finding,evidence,\
              observed_at,received_at,error_code,next_due_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
        )
        .bind(observation.execution_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(attempt_id)
        .bind(finding_name(observation.finding))
        .bind(&observation.evidence)
        .bind(observation.observed_at)
        .bind(observation.received_at)
        .bind(&observation.error_code)
        .bind(next_due_at)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        sqlx::query(
            "UPDATE publication_lookup_jobs SET lease_execution_id=NULL,lease_expires_at=NULL,\
             next_due_at=$1,last_error_code=$2 \
             WHERE operator_id=$3 AND tenant_id=$4 AND project_id=$5 AND attempt_id=$6",
        )
        .bind(next_due_at)
        .bind(&observation.error_code)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(attempt_id)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        let updated = get_locked(&mut tx, scope, attempt_id).await?;
        tx.commit().await.map_err(db)?;
        Ok(updated)
    }

    async fn get(
        &self,
        scope: &TenantScope,
        attempt_id: Uuid,
    ) -> Result<PublicationLookupJob, AppError> {
        let query = format!(
            "SELECT {JOB_FIELDS} FROM publication_lookup_jobs \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND attempt_id=$4"
        );
        let row = sqlx::query(&query)
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project(scope)?)
            .bind(attempt_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db)?
            .ok_or_else(|| AppError::not_found("lookup job not found"))?;
        job(&row)
    }

    async fn observations(
        &self,
        scope: &TenantScope,
        attempt_id: Uuid,
    ) -> Result<Vec<PublicationLookupObservation>, AppError> {
        // Reject other tenants' attempt IDs even when they have no observations.
        self.get(scope, attempt_id).await?;
        let rows = sqlx::query(
            "SELECT execution_id,attempt_id,finding,evidence,observed_at,received_at,error_code \
             FROM publication_lookup_observations WHERE operator_id=$1 AND tenant_id=$2 \
             AND project_id=$3 AND attempt_id=$4 ORDER BY received_at,execution_id",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(attempt_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(decode_observation).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::candidate;
    use chrono::{Duration, Utc};
    use geo_domain::{ChannelOutcome, ChannelOutcomeStatus, ChannelTargetInput};
    use sha2::{Digest, Sha256};
    use uuid::Uuid;

    #[test]
    fn canonical_hint_is_bound_to_original_send_and_public_asset_path() {
        let now = Utc::now();
        let attempt = Uuid::new_v4();
        let target = Uuid::new_v4();
        let account = Uuid::new_v4();
        let input = ChannelTargetInput::Publish {
            source_id: Uuid::new_v4(),
            source_version_id: Uuid::new_v4(),
            platform: "zhihu".into(),
            account_id: account,
            title: "  Example  title  ".into(),
            body: " Example \n body ".into(),
            body_sha256: "body-only-hash-not-title-body-hash".into(),
        };
        let digest = hex::encode(Sha256::digest(b"Example title\nExample body"));
        let evidence = |url: &str| {
            serde_json::json!({
                "kind":"publication_candidate","schema_version":"geo.publication.candidate.v1",
                "url":url,"expected_sha256":digest,"observed_at":now,
                "source":"post_submit_navigation","attempt_id":attempt,
                "target_id":target,"account_id":account,"connector_version":"browser.v1"
            })
        };
        let mut outcome = ChannelOutcome {
            status: ChannelOutcomeStatus::Unknown,
            detail: None,
            occurred_at: now,
            raw_answer: None,
            citations: vec![],
            public_url: None,
            screenshot_ref: None,
            connector_version: Some("browser.v1".into()),
            runner_evidence: vec![],
            fixture: false,
        };
        for url in [
            "https://www.zhihu.com/p/123456",
            "https://zhuanlan.zhihu.com/p/123456",
        ] {
            outcome.runner_evidence = vec![evidence(url)];
            assert_eq!(
                candidate(
                    Some(&outcome),
                    &input,
                    attempt,
                    target,
                    account,
                    now,
                    Some(now)
                ),
                Some(url.into())
            );
        }
        outcome.fixture = true;
        assert_eq!(
            candidate(
                Some(&outcome),
                &input,
                attempt,
                target,
                account,
                now,
                Some(now)
            ),
            None
        );
        outcome.fixture = false;
        outcome.runner_evidence = vec![evidence("https://www.zhihu.com/p/123456?token=x")];
        assert_eq!(
            candidate(
                Some(&outcome),
                &input,
                attempt,
                target,
                account,
                now,
                Some(now)
            ),
            None
        );
        outcome.runner_evidence = vec![evidence("https://www.zhihu.com/p/123456")];
        assert_eq!(
            candidate(
                Some(&outcome),
                &input,
                attempt,
                target,
                Uuid::new_v4(),
                now,
                Some(now)
            ),
            None
        );
        assert_eq!(
            candidate(
                Some(&outcome),
                &input,
                attempt,
                target,
                account,
                now + Duration::seconds(1),
                Some(now + Duration::seconds(2))
            ),
            None
        );
        outcome.runner_evidence[0]["expected_sha256"] = serde_json::json!("wrong");
        assert_eq!(
            candidate(
                Some(&outcome),
                &input,
                attempt,
                target,
                account,
                now,
                Some(now)
            ),
            None
        );
    }
}
