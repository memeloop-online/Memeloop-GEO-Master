use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, InitialSource, Project, ProjectCreate, ProjectId, ProjectPatch, ProjectRepository,
    ProjectSettings, ProjectStartAcceptance, ProjectStartCommand, ProjectStartView, ProjectStatus,
    ResourceMode, StartAcceptanceStatus, TenantScope, UpdateProject, previous_calendar_week_window,
    settings_hash, start_request_hash,
};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

/// PostgreSQL implementation of the tenant-scoped project repository.
#[derive(Clone)]
pub struct PgProjectRepository {
    pool: PgPool,
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
            transaction.commit().await.map_err(map_database_error)?;
            if hashes.0 == command.idempotency_key_hash && hashes.1 == command.request_hash {
                return Ok(existing);
            }
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
        let row = sqlx::query_as::<_, (i64, String, DateTime<Utc>, DateTime<Utc>, DateTime<Utc>)>(
            "SELECT config.project_revision, config.settings_hash, cycle.report_window_start_at,
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
            report_window_start_at: row.2,
            report_window_end_at: row.3,
            cutoff_at: row.4,
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
