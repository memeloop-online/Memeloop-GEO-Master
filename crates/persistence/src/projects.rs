use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, InitialSource, Project, ProjectCreate, ProjectId, ProjectPatch, ProjectRepository,
    ProjectSettings, ProjectStatus, ResourceMode, TenantScope, UpdateProject,
};
use serde_json::Value;
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
                 monthly_budget_minor, monitoring_reserve_percent, status, revision)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                       $11, $12, $13, $14, $15, $16, $17, $18)"#,
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
    product_name: String,
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
        let settings = ProjectSettings {
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
        }
        .validate()?;
        Ok(Project {
            id: ProjectId::from(row.project_id),
            operator_id: row.operator_id.into(),
            tenant_id: row.tenant_id.into(),
            slug: row.slug,
            display_name: row.display_name,
            settings,
            status,
            revision: row.revision,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

fn project_columns() -> &'static str {
    "project_id, operator_id, tenant_id, slug, display_name, brand_name, product_name, market, language, target_audience, competitors, initial_sources, resource_mode, budget_currency, monthly_budget_minor, monitoring_reserve_percent, status, revision, created_at, updated_at"
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
                updated_at = $16 WHERE project_id = $17 AND operator_id = $18 AND tenant_id = $19"#,
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
