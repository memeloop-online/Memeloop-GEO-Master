use async_trait::async_trait;
use chrono::Utc;
use geo_domain::{
    AppError, CreateQuestionSet, ErrorCode, QuestionDraft, QuestionIdentity, QuestionProjectState,
    QuestionPurpose, QuestionReference, QuestionRepository, QuestionRevision, QuestionSetPage,
    QuestionSetRecord, QuestionSetSummary, QuestionSetVersion, QuestionSetVersionPage,
    QuestionSetVersionSummary, ResolvedQuestion, ReviseQuestionSet, TenantScope,
    normalize_question_text, validate_page_limit,
};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Transaction};
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Clone)]
pub struct PgQuestionRepository {
    pool: PgPool,
}

impl PgQuestionRepository {
    pub fn from_database(database: &crate::Database) -> Self {
        Self {
            pool: database.pool().clone(),
        }
    }

    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn scoped_project(scope: &TenantScope) -> Result<Uuid, AppError> {
    scope
        .project_id
        .map(|id| id.as_uuid())
        .ok_or_else(|| AppError::forbidden("project scope required"))
}

fn database_error(error: sqlx::Error) -> AppError {
    if error
        .as_database_error()
        .is_some_and(|database| database.is_unique_violation())
    {
        AppError::conflict("question identity or version conflicts with existing data")
    } else {
        AppError::new(
            ErrorCode::DependencyUnavailable,
            "question storage unavailable",
        )
    }
}

fn stored<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, AppError> {
    serde_json::from_value(value)
        .map_err(|_| AppError::new(ErrorCode::Internal, "stored question version invalid"))
}

fn encoded<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, AppError> {
    serde_json::to_value(value)
        .map_err(|_| AppError::new(ErrorCode::Internal, "question version encoding failed"))
}

async fn begin_scoped(
    pool: &PgPool,
    scope: &TenantScope,
) -> Result<Transaction<'static, Postgres>, AppError> {
    let mut tx = pool.begin().await.map_err(database_error)?;
    crate::set_local_scope(&mut tx, scope)
        .await
        .map_err(database_error)?;
    Ok(tx)
}

fn question_summary(version: &QuestionSetVersion) -> QuestionSetSummary {
    QuestionSetSummary {
        id: version.question_set_id,
        name: version.name.clone(),
        current_version_id: version.id,
        current_revision: version.revision,
        question_count: version.questions.len() as u32,
        optimization_count: version.optimization_count,
        evaluation_count: version.evaluation_count,
    }
}

fn version_summary(version: &QuestionSetVersion) -> QuestionSetVersionSummary {
    QuestionSetVersionSummary {
        id: version.id,
        question_set_id: version.question_set_id,
        revision: version.revision,
        parent_version_id: version.parent_version_id,
        name: version.name.clone(),
        question_count: version.questions.len() as u32,
        optimization_count: version.optimization_count,
        evaluation_count: version.evaluation_count,
        split_policy_version: version.split_policy_version.clone(),
        created_at: version.created_at,
    }
}

fn purpose_column(purpose: QuestionPurpose) -> &'static str {
    match purpose {
        QuestionPurpose::Optimization => "optimization",
        QuestionPurpose::FrozenEvaluation => "frozen_evaluation",
    }
}

fn alias_digest(normalized_text: &str) -> String {
    hex::encode(Sha256::digest(normalized_text.as_bytes()))
}

async fn version_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    set_id: Uuid,
    version_id: Uuid,
) -> Result<Option<QuestionSetVersion>, AppError> {
    let project = scoped_project(scope)?;
    let json: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT version_json FROM question_set_versions \
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 \
           AND question_set_id=$4 AND question_set_version_id=$5",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project)
    .bind(set_id)
    .bind(version_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?;
    json.map(stored).transpose()
}

async fn lock_project_and_registry(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
) -> Result<(Uuid, u64), AppError> {
    let project = scoped_project(scope)?;
    let found: Option<bool> = sqlx::query_scalar(
        "SELECT true FROM projects WHERE operator_id=$1 AND tenant_id=$2 \
         AND project_id=$3 FOR UPDATE",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project)
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?;
    if found.is_none() {
        return Err(AppError::not_found("project not found"));
    }
    sqlx::query(
        "INSERT INTO question_project_registries \
         (operator_id,tenant_id,project_id,split_seed) VALUES ($1,$2,$3,$4) \
         ON CONFLICT (operator_id,tenant_id,project_id) DO NOTHING",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project)
    .bind(Uuid::new_v4())
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    let (seed, count): (Uuid, i64) = sqlx::query_as(
        "SELECT split_seed,registered_count FROM question_project_registries \
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project)
    .fetch_one(&mut **tx)
    .await
    .map_err(database_error)?;
    let count = u64::try_from(count)
        .map_err(|_| AppError::new(ErrorCode::Internal, "invalid enrolled question count"))?;
    Ok((seed, count))
}

// Look up only aliases in this request and identities explicitly referenced by
// the current base version. No historical set/version JSON is scanned to decide
// whether a question has already been assigned to the frozen evaluation split.
async fn load_candidate_identities(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    drafts: &[QuestionDraft],
    base: Option<&QuestionSetVersion>,
) -> Result<Vec<QuestionIdentity>, AppError> {
    let project = scoped_project(scope)?;
    let normalized: Vec<_> = drafts
        .iter()
        .map(|draft| normalize_question_text(&draft.text))
        .collect();
    let digests: Vec<_> = normalized.iter().map(|text| alias_digest(text)).collect();
    let mut explicit: Vec<_> = drafts
        .iter()
        .filter_map(|draft| draft.question_id)
        .collect();
    explicit.extend(base.into_iter().flat_map(|version| {
        version
            .questions
            .iter()
            .map(|question| question.question_id)
    }));
    explicit.sort_unstable();
    explicit.dedup();
    let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT DISTINCT i.question_id,i.evaluation_split,i.split_policy_version \
         FROM question_identities i \
         WHERE i.operator_id=$1 AND i.tenant_id=$2 AND i.project_id=$3 \
           AND (i.question_id=ANY($4::uuid[]) OR EXISTS ( \
              SELECT 1 FROM question_identity_aliases a \
              WHERE a.operator_id=i.operator_id AND a.tenant_id=i.tenant_id \
                AND a.project_id=i.project_id AND a.question_id=i.question_id \
                AND a.normalized_digest=ANY($5::text[]) \
                AND a.normalized_text=ANY($6::text[])))",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project)
    .bind(&explicit)
    .bind(&digests)
    .bind(&normalized)
    .fetch_all(&mut **tx)
    .await
    .map_err(database_error)?;
    let ids: Vec<_> = rows.iter().map(|(id, _, _)| *id).collect();
    let aliases: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT question_id,normalized_text FROM question_identity_aliases \
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 \
           AND question_id=ANY($4::uuid[])",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project)
    .bind(ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(database_error)?;
    rows.into_iter()
        .map(|(id, purpose, split_policy_version)| {
            let purpose = match purpose.as_str() {
                "optimization" => QuestionPurpose::Optimization,
                "frozen_evaluation" => QuestionPurpose::FrozenEvaluation,
                _ => {
                    return Err(AppError::new(
                        ErrorCode::Internal,
                        "stored question split invalid",
                    ));
                }
            };
            Ok(QuestionIdentity {
                id,
                purpose,
                split_policy_version,
                aliases: aliases
                    .iter()
                    .filter(|(alias_id, _)| *alias_id == id)
                    .map(|(_, alias)| alias.clone())
                    .collect(),
            })
        })
        .collect()
}

async fn save_transition(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    original: &QuestionProjectState,
    updated: &QuestionProjectState,
    version: &QuestionSetVersion,
    key: &str,
    hash: &str,
) -> Result<(), AppError> {
    let project = scoped_project(scope)?;
    let operator = scope.operator_id.as_uuid();
    let tenant = scope.tenant_id.as_uuid();
    let old_ids: HashSet<_> = original
        .identities
        .iter()
        .map(|identity| identity.id)
        .collect();
    let old_aliases: HashSet<_> = original
        .identities
        .iter()
        .flat_map(|identity| {
            identity
                .aliases
                .iter()
                .map(move |alias| (identity.id, alias.as_str()))
        })
        .collect();
    for identity in &updated.identities {
        if !old_ids.contains(&identity.id) {
            sqlx::query(
                "INSERT INTO question_identities \
                 (operator_id,tenant_id,project_id,question_id,evaluation_split,\
                  split_policy_version,registered_at) VALUES ($1,$2,$3,$4,$5,$6,$7)",
            )
            .bind(operator)
            .bind(tenant)
            .bind(project)
            .bind(identity.id)
            .bind(purpose_column(identity.purpose))
            .bind(&identity.split_policy_version)
            .bind(version.created_at)
            .execute(&mut **tx)
            .await
            .map_err(database_error)?;
        }
        for alias in &identity.aliases {
            if !old_aliases.contains(&(identity.id, alias.as_str())) {
                sqlx::query(
                    "INSERT INTO question_identity_aliases \
                     (operator_id,tenant_id,project_id,normalized_digest,normalized_text,question_id) \
                     VALUES ($1,$2,$3,$4,$5,$6)",
                )
                .bind(operator)
                .bind(tenant)
                .bind(project)
                .bind(alias_digest(alias))
                .bind(alias)
                .bind(identity.id)
                .execute(&mut **tx)
                .await
                .map_err(database_error)?;
            }
        }
    }
    if original.sets.is_empty() {
        sqlx::query(
            "INSERT INTO question_sets \
             (operator_id,tenant_id,project_id,question_set_id,name,created_at,updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$6)",
        )
        .bind(operator)
        .bind(tenant)
        .bind(project)
        .bind(version.question_set_id)
        .bind(&version.name)
        .bind(version.created_at)
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
    }
    for revision in &version.questions {
        sqlx::query(
            "INSERT INTO question_revisions \
             (operator_id,tenant_id,project_id,question_id,question_revision_id,revision_json) \
             VALUES ($1,$2,$3,$4,$5,$6) \
             ON CONFLICT (operator_id,tenant_id,project_id,question_id,question_revision_id) \
             DO NOTHING",
        )
        .bind(operator)
        .bind(tenant)
        .bind(project)
        .bind(revision.question_id)
        .bind(revision.id)
        .bind(encoded(revision)?)
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
    }
    sqlx::query(
        "INSERT INTO question_set_versions \
         (operator_id,tenant_id,project_id,question_set_id,question_set_version_id,\
          revision,name,created_at,version_json) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
    )
    .bind(operator)
    .bind(tenant)
    .bind(project)
    .bind(version.question_set_id)
    .bind(version.id)
    .bind(i64::from(version.revision))
    .bind(&version.name)
    .bind(version.created_at)
    .bind(encoded(version)?)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    for (position, revision) in version.questions.iter().enumerate() {
        sqlx::query(
            "INSERT INTO question_version_members \
             (operator_id,tenant_id,project_id,question_set_id,question_set_version_id,\
              position,question_id,question_revision_id) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
        )
        .bind(operator)
        .bind(tenant)
        .bind(project)
        .bind(version.question_set_id)
        .bind(version.id)
        .bind(i32::try_from(position).map_err(|_| AppError::invalid_request("too many questions"))?)
        .bind(revision.question_id)
        .bind(revision.id)
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
    }
    sqlx::query(
        "UPDATE question_sets SET latest_version_id=$1,name=$2,updated_at=$3 \
         WHERE operator_id=$4 AND tenant_id=$5 AND project_id=$6 AND question_set_id=$7",
    )
    .bind(version.id)
    .bind(&version.name)
    .bind(version.created_at)
    .bind(operator)
    .bind(tenant)
    .bind(project)
    .bind(version.question_set_id)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    sqlx::query(
        "UPDATE question_project_registries SET registered_count=$1 \
         WHERE operator_id=$2 AND tenant_id=$3 AND project_id=$4",
    )
    .bind(
        i64::try_from(updated.enrolled_count)
            .map_err(|_| AppError::invalid_request("project question capacity exceeded"))?,
    )
    .bind(operator)
    .bind(tenant)
    .bind(project)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    sqlx::query(
        "INSERT INTO question_set_requests \
         (operator_id,tenant_id,project_id,idempotency_key,request_hash,\
          question_set_id,question_set_version_id) VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(operator)
    .bind(tenant)
    .bind(project)
    .bind(key)
    .bind(hash)
    .bind(version.question_set_id)
    .bind(version.id)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    Ok(())
}

async fn replay(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    key: &str,
    hash: &str,
) -> Result<Option<QuestionSetVersion>, AppError> {
    let project = scoped_project(scope)?;
    let row: Option<(String, Uuid, Uuid)> = sqlx::query_as(
        "SELECT request_hash,question_set_id,question_set_version_id \
         FROM question_set_requests WHERE operator_id=$1 AND tenant_id=$2 \
         AND project_id=$3 AND idempotency_key=$4",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project)
    .bind(key)
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?;
    let Some((stored_hash, set_id, version_id)) = row else {
        return Ok(None);
    };
    if stored_hash != hash {
        return Err(AppError::conflict(
            "idempotency key was used for another question change",
        ));
    }
    version_in_transaction(tx, scope, set_id, version_id)
        .await?
        .map(Some)
        .ok_or_else(|| AppError::new(ErrorCode::Internal, "stored question replay missing"))
}

#[async_trait]
impl QuestionRepository for PgQuestionRepository {
    async fn replay(
        &self,
        scope: &TenantScope,
        key: &str,
        request_hash: &str,
    ) -> Result<Option<QuestionSetVersion>, AppError> {
        let mut tx = begin_scoped(&self.pool, scope).await?;
        replay(&mut tx, scope, key, request_hash).await
    }

    async fn list_sets(
        &self,
        scope: &TenantScope,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<QuestionSetPage, AppError> {
        let max = validate_page_limit(limit)?;
        let project = scoped_project(scope)?;
        let json: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT v.version_json FROM question_sets s JOIN question_set_versions v \
             ON (v.operator_id,v.tenant_id,v.project_id,v.question_set_id,v.question_set_version_id)= \
                (s.operator_id,s.tenant_id,s.project_id,s.question_set_id,s.latest_version_id) \
             WHERE s.operator_id=$1 AND s.tenant_id=$2 AND s.project_id=$3 \
               AND ($4::uuid IS NULL OR s.question_set_id>$4) \
             ORDER BY s.question_set_id LIMIT $5",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project)
        .bind(after)
        .bind(i64::try_from(max + 1).expect("bounded page"))
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        let mut items: Vec<_> = json
            .into_iter()
            .map(stored::<QuestionSetVersion>)
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .map(question_summary)
            .collect();
        let next_cursor = (items.len() > max).then(|| items[max - 1].id);
        items.truncate(max);
        Ok(QuestionSetPage { items, next_cursor })
    }

    async fn create_set(
        &self,
        scope: &TenantScope,
        command: CreateQuestionSet,
    ) -> Result<QuestionSetVersion, AppError> {
        let hash = geo_domain::create_question_request_hash(&command)?;
        let mut tx = begin_scoped(&self.pool, scope).await?;
        let (seed, count) = lock_project_and_registry(&mut tx, scope).await?;
        if let Some(version) = replay(&mut tx, scope, &command.idempotency_key, &hash).await? {
            return Ok(version);
        }
        let identities =
            load_candidate_identities(&mut tx, scope, &command.questions, None).await?;
        let mut state = QuestionProjectState::from_parts(seed, count, identities, vec![], vec![]);
        let original = state.clone();
        let key = command.idempotency_key.clone();
        let version = state.create_set(command, Utc::now())?;
        save_transition(&mut tx, scope, &original, &state, &version, &key, &hash).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(version)
    }

    async fn list_versions(
        &self,
        scope: &TenantScope,
        set_id: Uuid,
        after_revision: Option<u32>,
        limit: u32,
    ) -> Result<QuestionSetVersionPage, AppError> {
        let max = validate_page_limit(limit)?;
        let project = scoped_project(scope)?;
        let exists: Option<bool> = sqlx::query_scalar(
            "SELECT true FROM question_sets WHERE operator_id=$1 AND tenant_id=$2 \
             AND project_id=$3 AND question_set_id=$4",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project)
        .bind(set_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)?;
        if exists.is_none() {
            return Err(AppError::not_found("question set not found"));
        }
        let json: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT version_json FROM question_set_versions \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 \
               AND question_set_id=$4 AND ($5::bigint IS NULL OR revision>$5) \
             ORDER BY revision LIMIT $6",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project)
        .bind(set_id)
        .bind(after_revision.map(i64::from))
        .bind(i64::try_from(max + 1).expect("bounded page"))
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        let mut items: Vec<_> = json
            .into_iter()
            .map(stored::<QuestionSetVersion>)
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .map(version_summary)
            .collect();
        let next_cursor = (items.len() > max).then(|| items[max - 1].revision);
        items.truncate(max);
        Ok(QuestionSetVersionPage { items, next_cursor })
    }

    async fn get_version(
        &self,
        scope: &TenantScope,
        set_id: Uuid,
        version_id: Uuid,
    ) -> Result<QuestionSetVersion, AppError> {
        let project = scoped_project(scope)?;
        let json: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT version_json FROM question_set_versions \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 \
               AND question_set_id=$4 AND question_set_version_id=$5",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project)
        .bind(set_id)
        .bind(version_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)?;
        stored(json.ok_or_else(|| AppError::not_found("question set version not found"))?)
    }

    async fn revise_set(
        &self,
        scope: &TenantScope,
        set_id: Uuid,
        command: ReviseQuestionSet,
    ) -> Result<QuestionSetVersion, AppError> {
        let hash = geo_domain::revise_question_request_hash(set_id, &command)?;
        let mut tx = begin_scoped(&self.pool, scope).await?;
        let (seed, count) = lock_project_and_registry(&mut tx, scope).await?;
        if let Some(version) = replay(&mut tx, scope, &command.idempotency_key, &hash).await? {
            return Ok(version);
        }
        let project = scoped_project(scope)?;
        let current_id: Option<Option<Uuid>> = sqlx::query_scalar(
            "SELECT latest_version_id FROM question_sets \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND question_set_id=$4",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project)
        .bind(set_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?;
        let current_id = current_id
            .ok_or_else(|| AppError::not_found("question set not found"))?
            .ok_or_else(|| {
                AppError::new(ErrorCode::Internal, "question set has no current version")
            })?;
        let current = version_in_transaction(&mut tx, scope, set_id, current_id)
            .await?
            .ok_or_else(|| {
                AppError::new(ErrorCode::Internal, "current question version missing")
            })?;
        let identities =
            load_candidate_identities(&mut tx, scope, &command.questions, Some(&current)).await?;
        let mut state = QuestionProjectState::from_parts(
            seed,
            count,
            identities,
            vec![QuestionSetRecord {
                id: set_id,
                versions: vec![current],
            }],
            vec![],
        );
        let original = state.clone();
        let key = command.idempotency_key.clone();
        let version = state.revise_set(set_id, command, Utc::now())?;
        save_transition(&mut tx, scope, &original, &state, &version, &key, &hash).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(version)
    }

    async fn resolve_question(
        &self,
        scope: &TenantScope,
        reference: QuestionReference,
    ) -> Result<ResolvedQuestion, AppError> {
        let project = scoped_project(scope)?;
        let row: Option<(serde_json::Value, serde_json::Value, String, String)> = sqlx::query_as(
            "SELECT v.version_json,r.revision_json,i.evaluation_split,i.split_policy_version \
             FROM question_version_members m \
             JOIN question_set_versions v \
               ON (v.operator_id,v.tenant_id,v.project_id,v.question_set_id,v.question_set_version_id)= \
                  (m.operator_id,m.tenant_id,m.project_id,m.question_set_id,m.question_set_version_id) \
             JOIN question_revisions r \
               ON (r.operator_id,r.tenant_id,r.project_id,r.question_id,r.question_revision_id)= \
                  (m.operator_id,m.tenant_id,m.project_id,m.question_id,m.question_revision_id) \
             JOIN question_identities i \
               ON (i.operator_id,i.tenant_id,i.project_id,i.question_id)= \
                  (m.operator_id,m.tenant_id,m.project_id,m.question_id) \
             WHERE m.operator_id=$1 AND m.tenant_id=$2 AND m.project_id=$3 \
               AND m.question_set_id=$4 AND m.question_set_version_id=$5 \
               AND m.question_id=$6 AND m.question_revision_id=$7",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project)
        .bind(reference.question_set_id)
        .bind(reference.question_set_version_id)
        .bind(reference.question_id)
        .bind(reference.question_revision_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)?;
        let (version_json, revision_json, split, policy_version) =
            row.ok_or_else(|| AppError::not_found("question is not a member of this version"))?;
        let version: QuestionSetVersion = stored(version_json)?;
        let revision: QuestionRevision = stored(revision_json)?;
        let state = QuestionProjectState::from_parts(
            Uuid::nil(),
            0,
            vec![],
            vec![QuestionSetRecord {
                id: version.question_set_id,
                versions: vec![version],
            }],
            vec![],
        );
        let resolved = state.resolve_question(reference)?;
        if resolved.revision != revision
            || purpose_column(resolved.binding.purpose) != split
            || resolved.binding.split_policy_version != policy_version
        {
            return Err(AppError::new(
                ErrorCode::Internal,
                "stored question membership is inconsistent",
            ));
        }
        Ok(resolved)
    }
}
