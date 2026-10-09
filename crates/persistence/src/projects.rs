use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, ContentProjectGuard, CycleReportView, InitialSource, PendingSuccessorCycle, Project,
    ProjectCreate, ProjectId, ProjectPatch, ProjectRepository, ProjectSettings,
    ProjectStartAcceptance, ProjectStartCommand, ProjectStartView, ProjectStatus, ResourceMode,
    StartAcceptanceStatus, TenantScope, UpdateProject, next_calendar_week_window,
    previous_calendar_week_window, settings_hash, start_request_hash,
};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

/// PostgreSQL implementation of the tenant-scoped project repository.
#[derive(Clone)]
pub struct PgProjectRepository {
    pool: PgPool,
}

#[derive(Debug, Clone)]
pub struct PendingContentCycle {
    pub scope: TenantScope,
    pub cycle_id: Uuid,
}

#[derive(Debug, Clone)]
pub struct ContentBootstrapLease {
    pub scope: TenantScope,
    pub cycle_id: Uuid,
    pub token: Uuid,
}

impl PgProjectRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn from_database(database: &crate::Database) -> Self {
        Self::new(database.pool().clone())
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Bounded keyset scan of active, current cycles with no content execution.
    /// A failed cycle cannot prevent enumeration of later cycles in this pass.
    pub async fn scan_pending_content_cycles_after(
        &self,
        after: Option<Uuid>,
        now: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<PendingContentCycle>, AppError> {
        if !(1..=1000).contains(&limit) {
            return Err(AppError::invalid_request("invalid cycle scan page size"));
        }
        let rows: Vec<(Uuid, Uuid, Uuid, Uuid)> = sqlx::query_as(
            "SELECT c.operator_id,c.tenant_id,c.project_id,c.cycle_id \
             FROM optimization_cycles c JOIN projects p \
               ON (p.operator_id,p.tenant_id,p.project_id)=(c.operator_id,c.tenant_id,c.project_id) \
             WHERE ($1::uuid IS NULL OR c.cycle_id>$1) AND p.current_cycle_id=c.cycle_id \
               AND p.status='active' \
               AND (c.content_bootstrap_expires_at IS NULL OR c.content_bootstrap_expires_at<=$2) \
               AND (c.content_bootstrap_retry_after IS NULL OR c.content_bootstrap_retry_after<=$2) \
               AND NOT EXISTS (SELECT 1 FROM content_executions e WHERE \
                 (e.operator_id,e.tenant_id,e.project_id,e.cycle_id)= \
                 (c.operator_id,c.tenant_id,c.project_id,c.cycle_id)) \
             ORDER BY c.cycle_id LIMIT $3",
        )
        .bind(after)
        .bind(now)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(map_database_error)?;
        Ok(rows
            .into_iter()
            .map(
                |(operator, tenant, project, cycle_id)| PendingContentCycle {
                    scope: TenantScope::new(operator.into(), tenant.into(), Some(project.into())),
                    cycle_id,
                },
            )
            .collect())
    }

    /// The project lock serializes with cycle advancement and pause. The
    /// cycle lock serializes bootstrap attempts across scanner replicas.
    pub async fn try_claim_content_bootstrap(
        &self,
        scope: &TenantScope,
        cycle_id: Uuid,
        now: DateTime<Utc>,
        ttl: chrono::Duration,
    ) -> Result<Option<ContentBootstrapLease>, AppError> {
        if ttl <= chrono::Duration::zero() {
            return Err(AppError::invalid_request(
                "bootstrap lease lifetime must be positive",
            ));
        }
        let project_id = scope
            .project_id
            .ok_or_else(|| AppError::invalid_request("project scope required"))?;
        let mut tx = self.pool.begin().await.map_err(map_database_error)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(map_database_error)?;
        let project: Option<(String, Option<Uuid>)> = sqlx::query_as(
            "SELECT status,current_cycle_id FROM projects \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 FOR UPDATE",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_database_error)?;
        if !matches!(project, Some((ref status, Some(current))) if status == "active" && current == cycle_id)
        {
            return Ok(None);
        }
        #[derive(sqlx::FromRow)]
        struct BootstrapTiming {
            content_bootstrap_expires_at: Option<DateTime<Utc>>,
            content_bootstrap_retry_after: Option<DateTime<Utc>>,
        }
        let row: Option<BootstrapTiming> = sqlx::query_as(
            "SELECT content_bootstrap_expires_at,content_bootstrap_retry_after \
             FROM optimization_cycles WHERE operator_id=$1 AND tenant_id=$2 \
             AND project_id=$3 AND cycle_id=$4 FOR UPDATE",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(cycle_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_database_error)?;
        let Some(BootstrapTiming {
            content_bootstrap_expires_at: expires,
            content_bootstrap_retry_after: retry,
        }) = row
        else {
            return Ok(None);
        };
        if expires.is_some_and(|time| time > now) || retry.is_some_and(|time| time > now) {
            return Ok(None);
        }
        let existing: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM content_executions WHERE operator_id=$1 \
             AND tenant_id=$2 AND project_id=$3 AND cycle_id=$4)",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(cycle_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_database_error)?;
        if existing {
            return Ok(None);
        }
        let token = Uuid::new_v4();
        sqlx::query(
            "UPDATE optimization_cycles SET content_bootstrap_token=$1, \
             content_bootstrap_expires_at=$2,content_bootstrap_retry_after=NULL, \
             content_bootstrap_attempts=content_bootstrap_attempts+1,content_bootstrap_error_code=NULL \
             WHERE cycle_id=$3 AND operator_id=$4 AND tenant_id=$5 AND project_id=$6",
        )
        .bind(token).bind(now + ttl).bind(cycle_id).bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid()).bind(project_id.as_uuid())
        .execute(&mut *tx).await.map_err(map_database_error)?;
        tx.commit().await.map_err(map_database_error)?;
        Ok(Some(ContentBootstrapLease {
            scope: scope.clone(),
            cycle_id,
            token,
        }))
    }

    /// Classify by stable error code only: diagnostic text may contain
    /// sensitive operational context and must not enter durable state.
    pub async fn finish_content_bootstrap(
        &self,
        lease: &ContentBootstrapLease,
        now: DateTime<Utc>,
        outcome: Result<(), geo_domain::ErrorCode>,
    ) -> Result<bool, AppError> {
        let project = lease
            .scope
            .project_id
            .ok_or_else(|| AppError::invalid_request("project scope required"))?;
        let (retry, code) = match outcome {
            Ok(()) => (None, None),
            Err(code) => {
                let (name, base): (&str, i64) = match code {
                    geo_domain::ErrorCode::NotReady
                    | geo_domain::ErrorCode::DependencyUnavailable => {
                        ("dependency_unavailable", 120)
                    }
                    geo_domain::ErrorCode::Conflict | geo_domain::ErrorCode::NotFound => {
                        ("input_unavailable", 1800)
                    }
                    geo_domain::ErrorCode::CapabilityMissing => ("capability_missing", 3600),
                    _ => ("invalid_input", 3600),
                };
                let attempts: i32 = sqlx::query_scalar(
                    "SELECT content_bootstrap_attempts FROM optimization_cycles \
                     WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND cycle_id=$4 AND content_bootstrap_token=$5",
                )
                .bind(lease.scope.operator_id.as_uuid()).bind(lease.scope.tenant_id.as_uuid())
                .bind(project.as_uuid()).bind(lease.cycle_id).bind(lease.token)
                .fetch_optional(&self.pool).await.map_err(map_database_error)?
                .unwrap_or_default();
                (
                    Some(
                        now + chrono::Duration::seconds(
                            (base * (1_i64 << attempts.saturating_sub(1).clamp(0, 5))).min(21_600),
                        ),
                    ),
                    Some(name),
                )
            }
        };
        let updated = sqlx::query(
            "UPDATE optimization_cycles SET content_bootstrap_token=NULL,content_bootstrap_expires_at=NULL, \
             content_bootstrap_retry_after=$1,content_bootstrap_error_code=$2 \
             WHERE operator_id=$3 AND tenant_id=$4 AND project_id=$5 AND cycle_id=$6 \
             AND content_bootstrap_token=$7 AND content_bootstrap_expires_at>$8",
        )
        .bind(retry).bind(code).bind(lease.scope.operator_id.as_uuid())
        .bind(lease.scope.tenant_id.as_uuid()).bind(project.as_uuid())
        .bind(lease.cycle_id).bind(lease.token).bind(now)
        .execute(&self.pool).await.map_err(map_database_error)?;
        Ok(updated.rows_affected() == 1)
    }

    /// Trusted, bounded crash-recovery selector. Call schedule_next_cycle
    /// with each returned scope and predecessor; the project row serializes
    /// concurrent scanners, and the predecessor unique key deduplicates.
    pub async fn list_pending_successor_cycles_after(
        &self,
        limit: i64,
        after: Option<Uuid>,
    ) -> Result<Vec<PendingSuccessorCycle>, AppError> {
        if !(1..=100).contains(&limit) {
            return Err(AppError::invalid_request("limit must be between 1 and 100"));
        }
        let rows: Vec<(Uuid, Uuid, Uuid, Uuid)> = sqlx::query_as(
            "SELECT c.operator_id, c.tenant_id, c.project_id, c.cycle_id
             FROM optimization_cycles c
             JOIN projects p ON p.operator_id=c.operator_id AND p.tenant_id=c.tenant_id
               AND p.project_id=c.project_id AND p.current_cycle_id=c.cycle_id
             WHERE p.status='active'
               AND ($1::uuid IS NULL OR c.cycle_id > $1)
               AND EXISTS (
                   SELECT 1 FROM report_snapshots r
                   WHERE r.operator_id=c.operator_id AND r.tenant_id=c.tenant_id
                     AND r.project_id=c.project_id AND r.cycle_id=c.cycle_id
                     AND r.revision=1
               )
               AND NOT EXISTS (
                   SELECT 1 FROM optimization_cycles next
                   WHERE next.operator_id=c.operator_id AND next.tenant_id=c.tenant_id
                     AND next.project_id=c.project_id AND next.previous_cycle_id=c.cycle_id
               )
             ORDER BY c.cycle_id
             LIMIT $2",
        )
        .bind(after)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_database_error)?;
        Ok(rows
            .into_iter()
            .map(
                |(operator, tenant, project, predecessor_cycle_id)| PendingSuccessorCycle {
                    scope: TenantScope::new(operator.into(), tenant.into(), Some(project.into())),
                    predecessor_cycle_id,
                },
            )
            .collect())
    }

    async fn insert_project(
        &self,
        scope: &TenantScope,
        input: ProjectCreate,
    ) -> Result<Project, AppError> {
        let mut transaction = self.pool.begin().await.map_err(map_database_error)?;
        crate::scope::set_local_scope(&mut transaction, scope)
            .await
            .map_err(map_database_error)?;
        let id = ProjectId::from(Uuid::new_v4());
        let slug = input
            .slug
            .clone()
            .unwrap_or_else(|| format!("project-{}", &id.to_string()[..8]));
        let project = Project::new(id, scope, slug, input.display_name.clone(), input.settings)?;
        let result = sqlx::query(
            r#"INSERT INTO projects
                (project_id, operator_id, tenant_id, slug, display_name,
                 brand_name, product_name, market, language, target_audience,
                 competitors, initial_sources, resource_mode, budget_currency,
                 monthly_budget_minor, monitoring_reserve_percent, status, revision, project_settings)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                       $11, $12, $13, $14, $15, $16, $17, $18, $19)"#,
        )
        .bind(project.id.as_uuid())
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(&project.slug)
        .bind(&project.display_name)
        .bind(&project.settings.brand_name)
        .bind(&project.settings.product_name)
        .bind(&project.settings.market)
        .bind(&project.settings.language)
        .bind(&project.settings.target_audience)
        .bind(serde_json::to_value(&project.settings.competitors).map_err(serialization_error)?)
        .bind(serde_json::to_value(&project.settings.initial_sources).map_err(serialization_error)?)
        .bind(project.settings.resource_mode.as_str())
        .bind(&project.settings.budget_currency)
        .bind(project.settings.monthly_budget_minor)
        .bind(i16::from(project.settings.monitoring_reserve_percent))
        .bind(project.status.as_str())
        .bind(project.revision)
        .bind(serde_json::to_value(&project.settings).map_err(serialization_error)?)
        .execute(&mut *transaction)
        .await
        .map_err(map_database_error)?;
        if result.rows_affected() != 1 {
            return Err(AppError::new(
                geo_domain::ErrorCode::Internal,
                "project was not created",
            ));
        }
        transaction.commit().await.map_err(map_database_error)?;
        Ok(project)
    }
}

#[derive(Debug, sqlx::FromRow)]
struct ProjectRow {
    project_id: Uuid,
    operator_id: Uuid,
    tenant_id: Uuid,
    slug: String,
    display_name: String,
    brand_name: String,
    product_name: Option<String>,
    market: String,
    language: String,
    target_audience: Option<String>,
    competitors: Value,
    initial_sources: Value,
    resource_mode: String,
    budget_currency: String,
    monthly_budget_minor: i64,
    monitoring_reserve_percent: i16,
    status: String,
    revision: i64,
    project_settings: Value,
    current_config_revision_id: Option<Uuid>,
    current_cycle_id: Option<Uuid>,
    start_operation_id: Option<Uuid>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<ProjectRow> for Project {
    type Error = AppError;

    fn try_from(row: ProjectRow) -> Result<Self, Self::Error> {
        let competitors =
            serde_json::from_value::<Vec<String>>(row.competitors).map_err(serialization_error)?;
        let initial_sources = serde_json::from_value::<Vec<InitialSource>>(row.initial_sources)
            .map_err(serialization_error)?;
        let resource_mode = match row.resource_mode.as_str() {
            "own" => ResourceMode::Own,
            "platform" => ResourceMode::Platform,
            "mixed" => ResourceMode::Mixed,
            other => {
                return Err(AppError::new(
                    geo_domain::ErrorCode::Internal,
                    format!("invalid resource_mode in database: {other}"),
                ));
            }
        };
        let status = ProjectStatus::parse(&row.status)?;
        let reserve = u8::try_from(row.monitoring_reserve_percent).map_err(|_| {
            AppError::new(
                geo_domain::ErrorCode::Internal,
                "invalid monitoring reserve in database",
            )
        })?;
        let legacy_settings = ProjectSettings {
            brand_name: row.brand_name,
            product_name: row.product_name,
            market: row.market,
            language: row.language,
            target_audience: row.target_audience,
            competitors,
            initial_sources,
            resource_mode,
            budget_currency: row.budget_currency,
            monthly_budget_minor: row.monthly_budget_minor,
            monitoring_reserve_percent: reserve,
            ..ProjectSettings::default()
        };
        let settings = if row.project_settings == json!({}) {
            legacy_settings.validate_draft()?
        } else {
            serde_json::from_value::<ProjectSettings>(row.project_settings)
                .map_err(serialization_error)?
                .validate_draft()?
        };
        Ok(Project {
            id: ProjectId::from(row.project_id),
            operator_id: row.operator_id.into(),
            tenant_id: row.tenant_id.into(),
            slug: row.slug,
            display_name: row.display_name,
            settings,
            status,
            revision: row.revision,
            current_config_revision_id: row.current_config_revision_id,
            current_cycle_id: row.current_cycle_id,
            start_operation_id: row.start_operation_id,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

fn project_columns() -> &'static str {
    "project_id, operator_id, tenant_id, slug, display_name, brand_name, product_name, market, language, target_audience, competitors, initial_sources, resource_mode, budget_currency, monthly_budget_minor, monitoring_reserve_percent, status, revision, project_settings, current_config_revision_id, current_cycle_id, start_operation_id, created_at, updated_at"
}

fn scope_predicate<'a>(builder: &mut QueryBuilder<'a, Postgres>, scope: &'a TenantScope) {
    builder
        .push("operator_id = ")
        .push_bind(scope.operator_id.as_uuid())
        .push(" AND tenant_id = ")
        .push_bind(scope.tenant_id.as_uuid());
}

#[async_trait]
impl ProjectRepository for PgProjectRepository {
    async fn hold_measurement_project<'a>(
        &'a self,
        scope: &TenantScope,
        project_id: ProjectId,
    ) -> Result<ContentProjectGuard<'a>, AppError> {
        if scope.project_id != Some(project_id) {
            return Err(AppError::forbidden("project is outside measurement scope"));
        }
        // PgSerpRepository verifies eligibility under a project row lock in
        // the transaction committing the first sending intent.
        Ok(ContentProjectGuard::transactional())
    }
    async fn hold_content_project<'a>(
        &'a self,
        scope: &TenantScope,
        project_id: ProjectId,
    ) -> Result<ContentProjectGuard<'a>, AppError> {
        if scope.project_id.is_some_and(|bound| bound != project_id) {
            return Err(AppError::not_found("project not found"));
        }
        // The PG content repository locks the project row and rechecks active
        // eligibility inside the content transaction; a separate read lock
        // would be released before that transaction can commit.
        Ok(ContentProjectGuard::transactional())
    }
    async fn list(&self, scope: &TenantScope) -> Result<Vec<Project>, AppError> {
        let mut transaction = self.pool.begin().await.map_err(map_database_error)?;
        crate::scope::set_local_scope(&mut transaction, scope)
            .await
            .map_err(map_database_error)?;
        let mut query = QueryBuilder::<Postgres>::new("SELECT ");
        query.push(project_columns()).push(" FROM projects WHERE ");
        scope_predicate(&mut query, scope);
        if let Some(project_id) = scope.project_id {
            query
                .push(" AND project_id = ")
                .push_bind(project_id.as_uuid());
        }
        query.push(" ORDER BY created_at ASC, project_id ASC");
        let rows = query
            .build_query_as::<ProjectRow>()
            .fetch_all(&mut *transaction)
            .await
            .map_err(map_database_error)?;
        transaction.commit().await.map_err(map_database_error)?;
        rows.into_iter().map(Project::try_from).collect()
    }

    async fn get(&self, scope: &TenantScope, id: ProjectId) -> Result<Option<Project>, AppError> {
        let mut transaction = self.pool.begin().await.map_err(map_database_error)?;
        crate::scope::set_local_scope(&mut transaction, scope)
            .await
            .map_err(map_database_error)?;
        let mut query = QueryBuilder::<Postgres>::new("SELECT ");
        query.push(project_columns()).push(" FROM projects WHERE ");
        scope_predicate(&mut query, scope);
        query.push(" AND project_id = ").push_bind(id.as_uuid());
        if let Some(project_id) = scope.project_id {
            query
                .push(" AND project_id = ")
                .push_bind(project_id.as_uuid());
        }
        let row = query
            .build_query_as::<ProjectRow>()
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_database_error)?;
        transaction.commit().await.map_err(map_database_error)?;
        row.map(Project::try_from).transpose()
    }

    async fn create(&self, scope: &TenantScope, input: ProjectCreate) -> Result<Project, AppError> {
        self.insert_project(scope, input).await
    }

    async fn update(
        &self,
        scope: &TenantScope,
        id: ProjectId,
        expected_revision: i64,
        patch: ProjectPatch,
    ) -> Result<UpdateProject, AppError> {
        let mut transaction = self.pool.begin().await.map_err(map_database_error)?;
        crate::scope::set_local_scope(&mut transaction, scope)
            .await
            .map_err(map_database_error)?;
        let mut query = QueryBuilder::<Postgres>::new("SELECT ");
        query.push(project_columns()).push(" FROM projects WHERE ");
        scope_predicate(&mut query, scope);
        query
            .push(" AND project_id = ")
            .push_bind(id.as_uuid())
            .push(" FOR UPDATE");
        let row = query
            .build_query_as::<ProjectRow>()
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_database_error)?
            .ok_or_else(|| AppError::not_found("project not found"))?;
        let current = Project::try_from(row)?;
        if current.revision != expected_revision {
            return Err(AppError::conflict(format!(
                "project revision conflict: expected {expected_revision}, current {}",
                current.revision
            )));
        }
        let mut updated = current.clone();
        patch.apply_to(&mut updated)?;
        let duplicate = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM projects WHERE operator_id = $1 AND tenant_id = $2 AND slug = $3 AND project_id <> $4)",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(&updated.slug)
        .bind(id.as_uuid())
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_database_error)?;
        if duplicate {
            return Err(AppError::conflict(
                "a project with this slug already exists in the tenant",
            ));
        }
        updated.revision += 1;
        updated.updated_at = Utc::now();
        sqlx::query(
            r#"UPDATE projects SET slug = $1, display_name = $2, brand_name = $3,
                product_name = $4, market = $5, language = $6, target_audience = $7,
                competitors = $8, initial_sources = $9, resource_mode = $10,
                budget_currency = $11, monthly_budget_minor = $12,
                monitoring_reserve_percent = $13, status = $14, revision = $15,
                updated_at = $16, project_settings = $17
                WHERE project_id = $18 AND operator_id = $19 AND tenant_id = $20"#,
        )
        .bind(&updated.slug)
        .bind(&updated.display_name)
        .bind(&updated.settings.brand_name)
        .bind(&updated.settings.product_name)
        .bind(&updated.settings.market)
        .bind(&updated.settings.language)
        .bind(&updated.settings.target_audience)
        .bind(serde_json::to_value(&updated.settings.competitors).map_err(serialization_error)?)
        .bind(serde_json::to_value(&updated.settings.initial_sources).map_err(serialization_error)?)
        .bind(updated.settings.resource_mode.as_str())
        .bind(&updated.settings.budget_currency)
        .bind(updated.settings.monthly_budget_minor)
        .bind(i16::from(updated.settings.monitoring_reserve_percent))
        .bind(updated.status.as_str())
        .bind(updated.revision)
        .bind(updated.updated_at)
        .bind(serde_json::to_value(&updated.settings).map_err(serialization_error)?)
        .bind(id.as_uuid())
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .execute(&mut *transaction)
        .await
        .map_err(map_database_error)?;
        transaction.commit().await.map_err(map_database_error)?;
        Ok(UpdateProject {
            project: updated,
            previous_revision: expected_revision,
        })
    }

    async fn start(
        &self,
        scope: &TenantScope,
        id: ProjectId,
        command: ProjectStartCommand,
    ) -> Result<ProjectStartAcceptance, AppError> {
        let mut transaction = self.pool.begin().await.map_err(map_database_error)?;
        crate::scope::set_local_scope(&mut transaction, scope)
            .await
            .map_err(map_database_error)?;
        let mut query = QueryBuilder::<Postgres>::new("SELECT ");
        query.push(project_columns()).push(" FROM projects WHERE ");
        scope_predicate(&mut query, scope);
        query
            .push(" AND project_id = ")
            .push_bind(id.as_uuid())
            .push(" FOR UPDATE");
        let project = query
            .build_query_as::<ProjectRow>()
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_database_error)?
            .ok_or_else(|| AppError::not_found("project not found"))
            .and_then(Project::try_from)?;

        // The project row lock serializes all same/different-key start calls.
        if let Some(existing) = load_start_acceptance(&mut transaction, scope, id).await? {
            let hashes = sqlx::query_as::<_, (String, String)>(
                "SELECT idempotency_key_hash, request_hash FROM project_start_records
                 WHERE operator_id = $1 AND tenant_id = $2 AND project_id = $3",
            )
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(id.as_uuid())
            .fetch_one(&mut *transaction)
            .await
            .map_err(map_database_error)?;
            if hashes.0 == command.idempotency_key_hash && hashes.1 == command.request_hash {
                // Replay the original accepted command, not a view whose
                // manifest states may have advanced since startup. GET start
                // remains the live read model.
                let result: Value = sqlx::query_scalar(
                    "SELECT result FROM operations
                     WHERE operation_id=$1 AND operator_id=$2 AND tenant_id=$3 AND project_id=$4",
                )
                .bind(existing.operation_id)
                .bind(scope.operator_id.as_uuid())
                .bind(scope.tenant_id.as_uuid())
                .bind(id.as_uuid())
                .fetch_one(&mut *transaction)
                .await
                .map_err(map_database_error)?;
                let original = serde_json::from_value(result).map_err(serialization_error)?;
                transaction.commit().await.map_err(map_database_error)?;
                return Ok(original);
            }
            transaction.commit().await.map_err(map_database_error)?;
            if hashes.0 == command.idempotency_key_hash {
                return Err(AppError::conflict(
                    "Idempotency-Key was already used with a different project start request",
                ));
            }
            return Err(AppError::conflict("project has already been started"));
        }
        if project.status != ProjectStatus::Draft {
            return Err(AppError::conflict("project has already been started"));
        }
        if project.revision != command.expected_revision {
            return Err(AppError::conflict(format!(
                "project revision conflict: expected {}, current {}",
                command.expected_revision, project.revision
            )));
        }
        let settings = project.settings.clone().validate_start()?;
        let computed_settings_hash = settings_hash(&settings)?;
        if computed_settings_hash != command.settings_hash
            || start_request_hash(id, command.expected_revision, &computed_settings_hash)
                != command.request_hash
        {
            return Err(AppError::conflict(
                "project settings changed while the start request was being prepared",
            ));
        }
        let (report_window_start_at, report_window_end_at, cutoff_at) =
            previous_calendar_week_window(
                &settings.report_timezone,
                &settings.report_schedule,
                Utc::now(),
            )?;
        let config_revision_id = Uuid::new_v4();
        let cycle_id = Uuid::new_v4();
        let document_manifest_id = Uuid::new_v4();
        let distribution_manifest_id = Uuid::new_v4();
        let workflow_run_id = Uuid::new_v4();
        let acceptance = ProjectStartAcceptance {
            operation_id: command.operation_id,
            cycle_id,
            config_revision_id,
            document_manifest: geo_domain::DocumentManifestAcceptance {
                manifest_id: document_manifest_id,
                revision: 1,
                state: "awaiting_knowledge".to_owned(),
                sealed: false,
                expected_count: None,
            },
            distribution_manifest: geo_domain::DistributionManifestAcceptance {
                manifest_id: distribution_manifest_id,
                revision: 1,
                state: "awaiting_documents".to_owned(),
                sealed: false,
                expected_count: None,
            },
            status: StartAcceptanceStatus::Accepted,
            operation_url: format!("/api/v1/operations/{}", command.operation_id),
        };
        let scope_values = (
            scope.operator_id.as_uuid(),
            scope.tenant_id.as_uuid(),
            id.as_uuid(),
        );
        sqlx::query(
            "INSERT INTO project_config_revisions
             (config_revision_id, operator_id, tenant_id, project_id, project_revision, settings, source_refs, settings_hash, estimate_snapshot)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,NULL)",
        )
        .bind(config_revision_id)
        .bind(scope_values.0)
        .bind(scope_values.1)
        .bind(scope_values.2)
        .bind(project.revision)
        .bind(serde_json::to_value(&settings).map_err(serialization_error)?)
        .bind(serde_json::to_value(&settings.initial_sources).map_err(serialization_error)?)
        .bind(&computed_settings_hash)
        .execute(&mut *transaction)
        .await
        .map_err(map_database_error)?;
        sqlx::query(
            "INSERT INTO optimization_cycles
             (cycle_id, operator_id, tenant_id, project_id, config_revision_id, state, report_timezone, report_window_start_at, report_window_end_at, cutoff_at)
             VALUES ($1,$2,$3,$4,$5,'awaiting_knowledge',$6,$7,$8,$9)",
        )
        .bind(cycle_id)
        .bind(scope_values.0)
        .bind(scope_values.1)
        .bind(scope_values.2)
        .bind(config_revision_id)
        .bind(&settings.report_timezone)
        .bind(report_window_start_at)
        .bind(report_window_end_at)
        .bind(cutoff_at)
        .execute(&mut *transaction)
        .await
        .map_err(map_database_error)?;
        let document_input_refs = json!({
            "config_revision_id": config_revision_id,
            "source_refs": settings.initial_sources,
            "document_scope": settings.document_scope
        });
        sqlx::query(
            "INSERT INTO document_manifests
             (manifest_id, operator_id, tenant_id, project_id, cycle_id, revision, state, sealed, expected_count, scope_hash, input_refs)
             VALUES ($1,$2,$3,$4,$5,1,'awaiting_knowledge',false,NULL,$6,$7)",
        )
        .bind(document_manifest_id)
        .bind(scope_values.0)
        .bind(scope_values.1)
        .bind(scope_values.2)
        .bind(cycle_id)
        .bind(&computed_settings_hash)
        .bind(document_input_refs)
        .execute(&mut *transaction)
        .await
        .map_err(map_database_error)?;
        let distribution_input_refs = json!({
            "document_manifest_id": document_manifest_id,
            "distribution_scope": settings.distribution_scope
        });
        sqlx::query(
            "INSERT INTO distribution_manifests
             (manifest_id, operator_id, tenant_id, project_id, cycle_id, document_manifest_id, revision, state, sealed, expected_count, scope_hash, input_refs)
             VALUES ($1,$2,$3,$4,$5,$6,1,'awaiting_documents',false,NULL,$7,$8)",
        )
        .bind(distribution_manifest_id)
        .bind(scope_values.0)
        .bind(scope_values.1)
        .bind(scope_values.2)
        .bind(cycle_id)
        .bind(document_manifest_id)
        .bind(&computed_settings_hash)
        .bind(distribution_input_refs)
        .execute(&mut *transaction)
        .await
        .map_err(map_database_error)?;
        sqlx::query(
            "INSERT INTO workflow_runs
             (workflow_run_id, operator_id, tenant_id, project_id, cycle_id, kind, state, input_refs)
             VALUES ($1,$2,$3,$4,$5,'project_start','queued',$6)",
        )
        .bind(workflow_run_id)
        .bind(scope_values.0)
        .bind(scope_values.1)
        .bind(scope_values.2)
        .bind(cycle_id)
        .bind(json!({"config_revision_id": config_revision_id}))
        .execute(&mut *transaction)
        .await
        .map_err(map_database_error)?;
        sqlx::query(
            "INSERT INTO operations
             (operation_id, operator_id, tenant_id, project_id, kind, status, result, error)
             VALUES ($1,$2,$3,$4,'project.start','queued',$5,NULL)",
        )
        .bind(command.operation_id)
        .bind(scope_values.0)
        .bind(scope_values.1)
        .bind(scope_values.2)
        .bind(serde_json::to_value(&acceptance).map_err(serialization_error)?)
        .execute(&mut *transaction)
        .await
        .map_err(map_database_error)?;
        sqlx::query(
            "INSERT INTO outbox_events
             (event_id, event_type, schema_version, operator_id, tenant_id, project_id, aggregate_id, aggregate_version, occurred_at, correlation_id, payload)
             VALUES ($1,'cycle.created',1,$2,$3,$4,$5,1,now(),$6,$7)",
        )
        .bind(Uuid::new_v4())
        .bind(scope_values.0)
        .bind(scope_values.1)
        .bind(scope_values.2)
        .bind(cycle_id)
        .bind(command.operation_id)
        .bind(serde_json::to_value(&acceptance).map_err(serialization_error)?)
        .execute(&mut *transaction)
        .await
        .map_err(map_database_error)?;
        sqlx::query(
            "UPDATE projects SET status = 'active', revision = revision + 1, updated_at = now(),
                 current_config_revision_id = $1, current_cycle_id = $2, start_operation_id = $3
             WHERE operator_id = $4 AND tenant_id = $5 AND project_id = $6",
        )
        .bind(config_revision_id)
        .bind(cycle_id)
        .bind(command.operation_id)
        .bind(scope_values.0)
        .bind(scope_values.1)
        .bind(scope_values.2)
        .execute(&mut *transaction)
        .await
        .map_err(map_database_error)?;
        sqlx::query(
            "INSERT INTO project_start_records
             (project_start_record_id, operator_id, tenant_id, project_id, operation_id, cycle_id, config_revision_id,
              document_manifest_id, distribution_manifest_id, idempotency_key_hash, request_hash)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
        )
        .bind(Uuid::new_v4())
        .bind(scope_values.0)
        .bind(scope_values.1)
        .bind(scope_values.2)
        .bind(command.operation_id)
        .bind(cycle_id)
        .bind(config_revision_id)
        .bind(document_manifest_id)
        .bind(distribution_manifest_id)
        .bind(command.idempotency_key_hash)
        .bind(command.request_hash)
        .execute(&mut *transaction)
        .await
        .map_err(map_database_error)?;
        transaction.commit().await.map_err(map_database_error)?;
        Ok(acceptance)
    }

    async fn get_start(
        &self,
        scope: &TenantScope,
        id: ProjectId,
    ) -> Result<Option<ProjectStartView>, AppError> {
        let mut transaction = self.pool.begin().await.map_err(map_database_error)?;
        crate::scope::set_local_scope(&mut transaction, scope)
            .await
            .map_err(map_database_error)?;
        let acceptance = load_start_acceptance(&mut transaction, scope, id).await?;
        let Some(acceptance) = acceptance else {
            transaction.commit().await.map_err(map_database_error)?;
            return Ok(None);
        };
        let row = sqlx::query_as::<_, (i64, String, String, DateTime<Utc>, DateTime<Utc>, DateTime<Utc>)>(
            "SELECT config.project_revision, config.settings_hash, cycle.report_timezone, cycle.report_window_start_at,
                    cycle.report_window_end_at, cycle.cutoff_at
             FROM project_start_records record
             JOIN project_config_revisions config ON config.config_revision_id = record.config_revision_id
             JOIN optimization_cycles cycle ON cycle.cycle_id = record.cycle_id
             WHERE record.operator_id = $1 AND record.tenant_id = $2 AND record.project_id = $3",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(id.as_uuid())
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_database_error)?;
        transaction.commit().await.map_err(map_database_error)?;
        Ok(Some(ProjectStartView {
            project_id: id,
            acceptance,
            requested_revision: row.0,
            settings_hash: row.1,
            report_timezone: row.2,
            report_window_start_at: row.3,
            report_window_end_at: row.4,
            cutoff_at: row.5,
        }))
    }

    async fn schedule_next_cycle(
        &self,
        scope: &TenantScope,
        project_id: ProjectId,
        predecessor_cycle_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<CycleReportView, AppError> {
        if scope.project_id.is_some_and(|id| id != project_id) {
            return Err(AppError::not_found("project not found"));
        }
        let mut tx = self.pool.begin().await.map_err(map_database_error)?;
        crate::scope::set_local_scope(&mut tx, scope)
            .await
            .map_err(map_database_error)?;
        let project: Option<(String, Option<Uuid>, i64, Value)> = sqlx::query_as(
            "SELECT status, current_cycle_id, revision, project_settings FROM projects
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 FOR UPDATE",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_database_error)?;
        let (status, current_id, project_revision, current_settings_value) =
            project.ok_or_else(|| AppError::not_found("project not found"))?;
        let predecessor: Option<(String, DateTime<Utc>, DateTime<Utc>, Uuid, Value, String)> =
            sqlx::query_as(
                "SELECT cycle.report_timezone, cycle.report_window_end_at, cycle.cutoff_at,
                    cycle.config_revision_id, config.settings, config.settings_hash
             FROM optimization_cycles cycle
             JOIN project_config_revisions config
               ON config.operator_id=cycle.operator_id AND config.tenant_id=cycle.tenant_id
              AND config.project_id=cycle.project_id
              AND config.config_revision_id=cycle.config_revision_id
             WHERE cycle.operator_id=$1 AND cycle.tenant_id=$2
               AND cycle.project_id=$3 AND cycle.cycle_id=$4",
            )
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project_id.as_uuid())
            .bind(predecessor_cycle_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_database_error)?;
        let (
            timezone,
            predecessor_end,
            predecessor_cutoff,
            predecessor_config_id,
            settings_value,
            predecessor_settings_hash,
        ) = predecessor.ok_or_else(|| AppError::not_found("predecessor cycle not found"))?;
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT cycle_id FROM optimization_cycles
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND previous_cycle_id=$4",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(predecessor_cycle_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_database_error)?;
        if let Some(cycle_id) = existing {
            tx.commit().await.map_err(map_database_error)?;
            return self
                .get_report_cycle(scope, project_id, cycle_id)
                .await?
                .ok_or_else(|| {
                    AppError::new(
                        geo_domain::ErrorCode::Internal,
                        "successor cycle disappeared",
                    )
                });
        }
        if status != "active" {
            return Err(AppError::conflict(
                "only active projects can advance cycles",
            ));
        }
        if current_id != Some(predecessor_cycle_id) {
            return Err(AppError::conflict("predecessor is not the current cycle"));
        }
        if now < predecessor_cutoff {
            return Err(AppError::conflict(
                "predecessor report cutoff has not passed",
            ));
        }
        let predecessor_settings: ProjectSettings =
            serde_json::from_value(settings_value).map_err(serialization_error)?;
        let current_settings: ProjectSettings =
            serde_json::from_value(current_settings_value).map_err(serialization_error)?;
        let (start, end, cutoff) = next_calendar_week_window(
            &timezone,
            &predecessor_settings.report_schedule,
            predecessor_end,
        )?;
        let cycle_id = Uuid::new_v4();
        let document_id = Uuid::new_v4();
        let distribution_id = Uuid::new_v4();
        let keys = (
            scope.operator_id.as_uuid(),
            scope.tenant_id.as_uuid(),
            project_id.as_uuid(),
        );
        let mut settings = predecessor_settings.clone();
        settings.distribution_scope = current_settings.distribution_scope;
        let (config_id, config_hash) = if settings != predecessor_settings {
            settings = settings.validate_start()?;
            let config_id = Uuid::new_v4();
            let config_hash = settings_hash(&settings)?;
            sqlx::query(
                "INSERT INTO project_config_revisions
                 (config_revision_id,operator_id,tenant_id,project_id,project_revision,
                  settings,source_refs,settings_hash,estimate_snapshot)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,NULL)",
            )
            .bind(config_id)
            .bind(keys.0)
            .bind(keys.1)
            .bind(keys.2)
            .bind(project_revision)
            .bind(serde_json::to_value(&settings).map_err(serialization_error)?)
            .bind(serde_json::to_value(&settings.initial_sources).map_err(serialization_error)?)
            .bind(&config_hash)
            .execute(&mut *tx)
            .await
            .map_err(map_database_error)?;
            (config_id, config_hash)
        } else {
            (predecessor_config_id, predecessor_settings_hash)
        };
        sqlx::query(
            "INSERT INTO optimization_cycles
             (cycle_id,operator_id,tenant_id,project_id,config_revision_id,previous_cycle_id,
              state,report_timezone,report_window_start_at,report_window_end_at,cutoff_at)
             VALUES ($1,$2,$3,$4,$5,$6,'awaiting_knowledge',$7,$8,$9,$10)",
        )
        .bind(cycle_id)
        .bind(keys.0)
        .bind(keys.1)
        .bind(keys.2)
        .bind(config_id)
        .bind(predecessor_cycle_id)
        .bind(&timezone)
        .bind(start)
        .bind(end)
        .bind(cutoff)
        .execute(&mut *tx)
        .await
        .map_err(map_database_error)?;
        sqlx::query(
            "INSERT INTO document_manifests
             (manifest_id,operator_id,tenant_id,project_id,cycle_id,revision,state,sealed,
              expected_count,scope_hash,input_refs)
             VALUES ($1,$2,$3,$4,$5,1,'awaiting_knowledge',false,NULL,$6,$7)",
        )
        .bind(document_id)
        .bind(keys.0)
        .bind(keys.1)
        .bind(keys.2)
        .bind(cycle_id)
        .bind(&config_hash)
        .bind(
            json!({"config_revision_id":config_id,"source_refs":settings.initial_sources,
                     "document_scope":settings.document_scope}),
        )
        .execute(&mut *tx)
        .await
        .map_err(map_database_error)?;
        sqlx::query(
            "INSERT INTO distribution_manifests
             (manifest_id,operator_id,tenant_id,project_id,cycle_id,document_manifest_id,
              revision,state,sealed,expected_count,scope_hash,input_refs)
             VALUES ($1,$2,$3,$4,$5,$6,1,'awaiting_documents',false,NULL,$7,$8)",
        )
        .bind(distribution_id).bind(keys.0).bind(keys.1).bind(keys.2)
        .bind(cycle_id).bind(document_id).bind(&config_hash)
        .bind(json!({"document_manifest_id":document_id,"distribution_scope":settings.distribution_scope}))
        .execute(&mut *tx).await.map_err(map_database_error)?;
        sqlx::query(
            "UPDATE projects SET current_cycle_id=$1,current_config_revision_id=$2,
                    revision=revision+1,updated_at=$3
             WHERE operator_id=$4 AND tenant_id=$5 AND project_id=$6 AND current_cycle_id=$7",
        )
        .bind(cycle_id)
        .bind(config_id)
        .bind(now)
        .bind(keys.0)
        .bind(keys.1)
        .bind(keys.2)
        .bind(predecessor_cycle_id)
        .execute(&mut *tx)
        .await
        .map_err(map_database_error)?;
        tx.commit().await.map_err(map_database_error)?;
        self.get_report_cycle(scope, project_id, cycle_id)
            .await?
            .ok_or_else(|| {
                AppError::new(
                    geo_domain::ErrorCode::Internal,
                    "successor cycle disappeared",
                )
            })
    }

    async fn get_current_cycle(
        &self,
        scope: &TenantScope,
        project_id: ProjectId,
    ) -> Result<Option<CycleReportView>, AppError> {
        let Some(project) = self.get(scope, project_id).await? else {
            return Ok(None);
        };
        match project.current_cycle_id {
            Some(cycle_id) => self.get_report_cycle(scope, project_id, cycle_id).await,
            None => Ok(None),
        }
    }

    async fn get_cycle_settings(
        &self,
        scope: &TenantScope,
        project_id: ProjectId,
        cycle_id: Uuid,
    ) -> Result<Option<ProjectSettings>, AppError> {
        if scope.project_id.is_some_and(|id| id != project_id) {
            return Ok(None);
        }
        let mut transaction = self.pool.begin().await.map_err(map_database_error)?;
        crate::scope::set_local_scope(&mut transaction, scope)
            .await
            .map_err(map_database_error)?;
        let settings: Option<Value> = sqlx::query_scalar(
            "SELECT config.settings FROM optimization_cycles cycle
             JOIN project_config_revisions config
               ON config.operator_id=cycle.operator_id AND config.tenant_id=cycle.tenant_id
              AND config.project_id=cycle.project_id
              AND config.config_revision_id=cycle.config_revision_id
             WHERE cycle.operator_id=$1 AND cycle.tenant_id=$2
               AND cycle.project_id=$3 AND cycle.cycle_id=$4",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(cycle_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(map_database_error)?;
        transaction.commit().await.map_err(map_database_error)?;
        settings
            .map(|value| serde_json::from_value(value).map_err(serialization_error))
            .transpose()
    }

    async fn get_report_cycle(
        &self,
        scope: &TenantScope,
        project_id: ProjectId,
        cycle_id: Uuid,
    ) -> Result<Option<CycleReportView>, AppError> {
        #[derive(sqlx::FromRow)]
        struct Row {
            report_timezone: String,
            report_window_start_at: DateTime<Utc>,
            report_window_end_at: DateTime<Utc>,
            cutoff_at: DateTime<Utc>,
            document_manifest_id: Option<Uuid>,
            document_revision: Option<i32>,
            document_state: Option<String>,
            document_sealed: Option<bool>,
            document_expected_count: Option<i64>,
            distribution_manifest_id: Option<Uuid>,
            distribution_revision: Option<i32>,
            distribution_state: Option<String>,
            distribution_sealed: Option<bool>,
            distribution_expected_count: Option<i64>,
        }
        let mut transaction = self.pool.begin().await.map_err(map_database_error)?;
        crate::scope::set_local_scope(&mut transaction, scope)
            .await
            .map_err(map_database_error)?;
        let row = sqlx::query_as::<_, Row>(
            r#"SELECT cycle.report_timezone, cycle.report_window_start_at,
                      cycle.report_window_end_at, cycle.cutoff_at,
                      document.manifest_id AS document_manifest_id,
                      document.revision AS document_revision,
                      document.state AS document_state,
                      document.sealed AS document_sealed,
                      document.expected_count AS document_expected_count,
                      distribution.manifest_id AS distribution_manifest_id,
                      distribution.revision AS distribution_revision,
                      distribution.state AS distribution_state,
                      distribution.sealed AS distribution_sealed,
                      distribution.expected_count AS distribution_expected_count
               FROM optimization_cycles cycle
               LEFT JOIN LATERAL (
                   SELECT * FROM document_manifests
                   WHERE operator_id=cycle.operator_id AND tenant_id=cycle.tenant_id
                     AND project_id=cycle.project_id AND cycle_id=cycle.cycle_id
                   ORDER BY revision DESC LIMIT 1
               ) document ON true
               LEFT JOIN LATERAL (
                   SELECT * FROM distribution_manifests
                   WHERE operator_id=cycle.operator_id AND tenant_id=cycle.tenant_id
                     AND project_id=cycle.project_id AND cycle_id=cycle.cycle_id
                   ORDER BY revision DESC LIMIT 1
               ) distribution ON true
               WHERE cycle.operator_id=$1 AND cycle.tenant_id=$2
                 AND cycle.project_id=$3 AND cycle.cycle_id=$4"#,
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(cycle_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(map_database_error)?;
        transaction.commit().await.map_err(map_database_error)?;
        Ok(row.map(|row| CycleReportView {
            project_id,
            cycle_id,
            report_timezone: row.report_timezone,
            report_window_start_at: row.report_window_start_at,
            report_window_end_at: row.report_window_end_at,
            cutoff_at: row.cutoff_at,
            document_manifest: row.document_manifest_id.map(|manifest_id| {
                geo_domain::DocumentManifestAcceptance {
                    manifest_id,
                    revision: row.document_revision.unwrap_or_default(),
                    state: row.document_state.unwrap_or_default(),
                    sealed: row.document_sealed.unwrap_or(false),
                    expected_count: row.document_expected_count,
                }
            }),
            distribution_manifest: row.distribution_manifest_id.map(|manifest_id| {
                geo_domain::DistributionManifestAcceptance {
                    manifest_id,
                    revision: row.distribution_revision.unwrap_or_default(),
                    state: row.distribution_state.unwrap_or_default(),
                    sealed: row.distribution_sealed.unwrap_or(false),
                    expected_count: row.distribution_expected_count,
                }
            }),
        }))
    }
}

async fn load_start_acceptance(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    scope: &TenantScope,
    project_id: ProjectId,
) -> Result<Option<ProjectStartAcceptance>, AppError> {
    #[derive(sqlx::FromRow)]
    struct Row {
        operation_id: Uuid,
        cycle_id: Uuid,
        config_revision_id: Uuid,
        document_manifest_id: Uuid,
        distribution_manifest_id: Uuid,
        document_state: String,
        document_sealed: bool,
        document_expected_count: Option<i64>,
        distribution_state: String,
        distribution_sealed: bool,
        distribution_expected_count: Option<i64>,
    }
    let row = sqlx::query_as::<_, Row>(
        "SELECT record.operation_id, record.cycle_id, record.config_revision_id,
                record.document_manifest_id, record.distribution_manifest_id,
                document.state AS document_state, document.sealed AS document_sealed,
                document.expected_count AS document_expected_count,
                distribution.state AS distribution_state, distribution.sealed AS distribution_sealed,
                distribution.expected_count AS distribution_expected_count
         FROM project_start_records record
         JOIN document_manifests document ON document.manifest_id = record.document_manifest_id
         JOIN distribution_manifests distribution ON distribution.manifest_id = record.distribution_manifest_id
         WHERE record.operator_id = $1 AND record.tenant_id = $2 AND record.project_id = $3",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project_id.as_uuid())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(map_database_error)?;
    Ok(row.map(|row| ProjectStartAcceptance {
        operation_id: row.operation_id,
        cycle_id: row.cycle_id,
        config_revision_id: row.config_revision_id,
        document_manifest: geo_domain::DocumentManifestAcceptance {
            manifest_id: row.document_manifest_id,
            revision: 1,
            state: row.document_state,
            sealed: row.document_sealed,
            expected_count: row.document_expected_count,
        },
        distribution_manifest: geo_domain::DistributionManifestAcceptance {
            manifest_id: row.distribution_manifest_id,
            revision: 1,
            state: row.distribution_state,
            sealed: row.distribution_sealed,
            expected_count: row.distribution_expected_count,
        },
        status: StartAcceptanceStatus::Accepted,
        operation_url: format!("/api/v1/operations/{}", row.operation_id),
    }))
}

fn serialization_error(error: impl std::fmt::Display) -> AppError {
    AppError::new(
        geo_domain::ErrorCode::Internal,
        format!("project serialization failed: {error}"),
    )
}

fn map_database_error(error: sqlx::Error) -> AppError {
    if let sqlx::Error::Database(database) = &error
        && database.code().as_deref() == Some("23505")
    {
        return AppError::conflict("a project with this slug already exists in the tenant");
    }
    AppError::new(
        geo_domain::ErrorCode::DependencyUnavailable,
        "project persistence is unavailable",
    )
}
