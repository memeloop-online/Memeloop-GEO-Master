use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use tokio::sync::RwLock;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{AppError, OperatorId, ProjectId, TenantId, TenantScope};

/// Stable development-only identities used by the explicitly in-memory app
/// mode. They are intentionally not generated from user input.
pub const DEVELOPMENT_OPERATOR_ID: OperatorId =
    OperatorId(Uuid::from_u128(0x00000000000040008000000000000001));
pub const DEVELOPMENT_TENANT_ID: TenantId =
    TenantId(Uuid::from_u128(0x00000000000040008000000000000002));
pub const DEVELOPMENT_PROJECT_ID: ProjectId =
    ProjectId(Uuid::from_u128(0x00000000000040008000000000000003));

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Operator {
    pub id: OperatorId,
    pub slug: String,
    pub display_name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Operator {
    pub fn new(
        id: OperatorId,
        slug: impl Into<String>,
        display_name: impl Into<String>,
    ) -> Result<Self, AppError> {
        let slug = validate_text("slug", slug.into(), 100)?;
        let display_name = validate_text("display_name", display_name.into(), 200)?;
        let now = Utc::now();
        Ok(Self {
            id,
            slug,
            display_name,
            created_at: now,
            updated_at: now,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Tenant {
    pub id: TenantId,
    pub operator_id: OperatorId,
    pub slug: String,
    pub display_name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Tenant {
    pub fn new(
        id: TenantId,
        operator_id: OperatorId,
        slug: impl Into<String>,
        display_name: impl Into<String>,
    ) -> Result<Self, AppError> {
        let slug = validate_text("slug", slug.into(), 100)?;
        let display_name = validate_text("display_name", display_name.into(), 200)?;
        let now = Utc::now();
        Ok(Self {
            id,
            operator_id,
            slug,
            display_name,
            created_at: now,
            updated_at: now,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProjectStatus {
    #[default]
    Draft,
    Active,
    Paused,
    Archived,
}

impl ProjectStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Archived => "archived",
        }
    }

    pub fn parse(value: &str) -> Result<Self, AppError> {
        match value {
            "draft" => Ok(Self::Draft),
            "active" => Ok(Self::Active),
            "paused" => Ok(Self::Paused),
            "archived" => Ok(Self::Archived),
            _ => Err(AppError::invalid_request(format!(
                "invalid project status: {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ResourceMode {
    Own,
    Platform,
    Mixed,
}

impl ResourceMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Own => "own",
            Self::Platform => "platform",
            Self::Mixed => "mixed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProjectSettings {
    pub brand_name: String,
    pub product_name: String,
    pub market: String,
    pub language: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_audience: Option<String>,
    #[serde(default)]
    pub competitors: Vec<String>,
    #[serde(default)]
    pub initial_sources: Vec<InitialSource>,
    pub resource_mode: ResourceMode,
    #[serde(default = "default_budget_currency")]
    pub budget_currency: String,
    pub monthly_budget_minor: i64,
    pub monitoring_reserve_percent: u8,
}

impl ProjectSettings {
    pub fn validate(self) -> Result<Self, AppError> {
        let brand_name = validate_text("brand_name", self.brand_name, 200)?;
        let product_name = validate_text("product_name", self.product_name, 200)?;
        let market = validate_text("market", self.market, 100)?;
        let language = validate_text("language", self.language, 50)?;
        let target_audience = self
            .target_audience
            .map(|value| validate_text("target_audience", value, 200))
            .transpose()?;
        if self.competitors.len() > 5 {
            return Err(
                AppError::invalid_request("competitors must contain at most 5 entries")
                    .with_details(json!({ "field": "competitors", "max": 5 })),
            );
        }
        let mut competitors = Vec::with_capacity(self.competitors.len());
        for competitor in self.competitors {
            competitors.push(validate_text("competitors", competitor, 200)?);
        }
        if self.initial_sources.len() > 100 {
            return Err(AppError::invalid_request(
                "initial_sources must contain at most 100 entries",
            )
            .with_details(json!({ "field": "initial_sources", "max": 100 })));
        }
        let initial_sources = self
            .initial_sources
            .into_iter()
            .map(InitialSource::validate)
            .collect::<Result<Vec<_>, _>>()?;
        if self.monthly_budget_minor < 0 {
            return Err(AppError::invalid_request(
                "monthly_budget_minor must be non-negative",
            ));
        }
        let budget_currency = self.budget_currency.trim().to_ascii_uppercase();
        if budget_currency.len() != 3
            || !budget_currency
                .bytes()
                .all(|byte| byte.is_ascii_uppercase())
        {
            return Err(AppError::invalid_request(
                "budget_currency must be a three-letter ISO 4217 code",
            ));
        }
        if self.monitoring_reserve_percent > 100 {
            return Err(AppError::invalid_request(
                "monitoring_reserve_percent must be between 0 and 100",
            ));
        }
        Ok(Self {
            brand_name,
            product_name,
            market,
            language,
            target_audience,
            competitors,
            initial_sources,
            resource_mode: self.resource_mode,
            budget_currency,
            monthly_budget_minor: self.monthly_budget_minor,
            monitoring_reserve_percent: self.monitoring_reserve_percent,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum InitialSourceKind {
    Url,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum InitialSourceVisibility {
    Public,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InitialSource {
    pub kind: InitialSourceKind,
    pub value: String,
    pub visibility: InitialSourceVisibility,
}

impl InitialSource {
    fn validate(self) -> Result<Self, AppError> {
        let value = validate_text("initial_sources.value", self.value, 2_000_000)?;
        if matches!(self.kind, InitialSourceKind::Url)
            && !(value.starts_with("http://") || value.starts_with("https://"))
        {
            return Err(AppError::invalid_request(
                "initial_sources URL must start with http:// or https://",
            ));
        }
        Ok(Self { value, ..self })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Project {
    pub id: ProjectId,
    pub operator_id: OperatorId,
    pub tenant_id: TenantId,
    pub slug: String,
    pub display_name: String,
    pub settings: ProjectSettings,
    pub status: ProjectStatus,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Project {
    pub fn new(
        id: ProjectId,
        scope: &TenantScope,
        slug: impl Into<String>,
        display_name: impl Into<String>,
        settings: ProjectSettings,
    ) -> Result<Self, AppError> {
        if scope.project_id.is_some_and(|project_id| project_id != id) {
            return Err(AppError::invalid_request(
                "project scope does not match project id",
            ));
        }
        let now = Utc::now();
        Ok(Self {
            id,
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            slug: validate_text("slug", slug.into(), 100)?,
            display_name: validate_text("display_name", display_name.into(), 200)?,
            settings: settings.validate()?,
            status: ProjectStatus::Draft,
            revision: 1,
            created_at: now,
            updated_at: now,
        })
    }

    pub fn scope(&self) -> TenantScope {
        TenantScope::new(self.operator_id, self.tenant_id, Some(self.id))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProjectCreate {
    pub slug: Option<String>,
    pub display_name: String,
    pub settings: ProjectSettings,
}

/// Alias kept for callers that use the domain command name from the API.
pub type CreateProject = ProjectCreate;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProjectPatch {
    pub slug: Option<String>,
    pub display_name: Option<String>,
    pub brand_name: Option<String>,
    pub product_name: Option<String>,
    pub market: Option<String>,
    pub language: Option<String>,
    pub target_audience: Option<String>,
    #[serde(skip)]
    pub clear_target_audience: bool,
    pub competitors: Option<Vec<String>>,
    pub initial_sources: Option<Vec<InitialSource>>,
    pub resource_mode: Option<ResourceMode>,
    pub budget_currency: Option<String>,
    pub monthly_budget_minor: Option<i64>,
    pub monitoring_reserve_percent: Option<u8>,
    pub status: Option<ProjectStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateProject {
    pub project: Project,
    pub previous_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProjectPage {
    pub items: Vec<Project>,
    pub next_cursor: Option<String>,
}

impl ProjectPatch {
    pub fn apply_to(&self, project: &mut Project) -> Result<(), AppError> {
        if let Some(slug) = &self.slug {
            project.slug = validate_text("slug", slug.clone(), 100)?;
        }
        if let Some(display_name) = &self.display_name {
            project.display_name = validate_text("display_name", display_name.clone(), 200)?;
        }
        let mut settings = project.settings.clone();
        if let Some(value) = &self.brand_name {
            settings.brand_name = value.clone();
        }
        if let Some(value) = &self.product_name {
            settings.product_name = value.clone();
        }
        if let Some(value) = &self.market {
            settings.market = value.clone();
        }
        if let Some(value) = &self.language {
            settings.language = value.clone();
        }
        if self.clear_target_audience {
            settings.target_audience = None;
        } else if let Some(value) = &self.target_audience {
            settings.target_audience = Some(value.clone());
        }
        if let Some(value) = &self.competitors {
            settings.competitors = value.clone();
        }
        if let Some(value) = &self.initial_sources {
            settings.initial_sources = value.clone();
        }
        if let Some(value) = self.resource_mode {
            settings.resource_mode = value;
        }
        if let Some(value) = &self.budget_currency {
            settings.budget_currency = value.clone();
        }
        if let Some(value) = self.monthly_budget_minor {
            settings.monthly_budget_minor = value;
        }
        if let Some(value) = self.monitoring_reserve_percent {
            settings.monitoring_reserve_percent = value;
        }
        project.settings = settings.validate()?;
        if let Some(status) = self.status {
            project.status = status;
        }
        Ok(())
    }
}

#[async_trait]
pub trait ProjectRepository: Send + Sync {
    async fn list(&self, scope: &TenantScope) -> Result<Vec<Project>, AppError>;
    async fn list_page(
        &self,
        scope: &TenantScope,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<ProjectPage, AppError> {
        if !(1..=100).contains(&limit) {
            return Err(AppError::invalid_request("limit must be between 1 and 100"));
        }
        let cursor = match cursor {
            None => None,
            Some("") => {
                return Err(AppError::invalid_request("cursor must not be empty"));
            }
            Some(value) => Some(parse_cursor(value)?),
        };
        let mut projects = self.list(scope).await?;
        projects.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then(left.id.cmp(&right.id))
        });
        if let Some(cursor) = cursor {
            projects.retain(|project| {
                (project.created_at.timestamp_millis(), project.id.as_uuid()) > cursor
            });
        }
        let has_more = projects.len() > limit;
        projects.truncate(limit);
        let next_cursor = has_more
            .then(|| projects.last().map(format_cursor))
            .flatten();
        Ok(ProjectPage {
            items: projects,
            next_cursor,
        })
    }
    async fn get(&self, scope: &TenantScope, id: ProjectId) -> Result<Option<Project>, AppError>;
    async fn create(&self, scope: &TenantScope, input: ProjectCreate) -> Result<Project, AppError>;
    async fn update(
        &self,
        scope: &TenantScope,
        id: ProjectId,
        expected_revision: i64,
        patch: ProjectPatch,
    ) -> Result<UpdateProject, AppError>;
}

fn format_cursor(project: &Project) -> String {
    format!("{}:{}", project.created_at.timestamp_millis(), project.id)
}

fn parse_cursor(value: &str) -> Result<(i64, Uuid), AppError> {
    let (timestamp, id) = value
        .split_once(':')
        .ok_or_else(|| AppError::invalid_request("cursor must be timestamp:id"))?;
    let timestamp = timestamp
        .parse()
        .map_err(|_| AppError::invalid_request("cursor timestamp is invalid"))?;
    let id = id
        .parse()
        .map_err(|_| AppError::invalid_request("cursor project id is invalid"))?;
    Ok((timestamp, id))
}

#[derive(Debug, Default)]
pub struct MemoryProjectRepository {
    projects: RwLock<HashMap<ProjectId, Project>>,
}

impl MemoryProjectRepository {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn insert(&self, project: Project) -> Result<(), AppError> {
        let mut projects = self.projects.write().await;
        if projects.values().any(|candidate| {
            candidate.operator_id == project.operator_id
                && candidate.tenant_id == project.tenant_id
                && candidate.slug == project.slug
                && candidate.id != project.id
        }) {
            return Err(AppError::conflict(
                "a project with this slug already exists in the tenant",
            ));
        }
        projects.insert(project.id, project);
        Ok(())
    }
}

#[async_trait]
impl ProjectRepository for MemoryProjectRepository {
    async fn list(&self, scope: &TenantScope) -> Result<Vec<Project>, AppError> {
        let mut result = self
            .projects
            .read()
            .await
            .values()
            .filter(|project| {
                project.operator_id == scope.operator_id
                    && project.tenant_id == scope.tenant_id
                    && scope.project_id.is_none_or(|id| id == project.id)
            })
            .cloned()
            .collect::<Vec<_>>();
        result.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then(left.id.cmp(&right.id))
        });
        Ok(result)
    }

    async fn get(&self, scope: &TenantScope, id: ProjectId) -> Result<Option<Project>, AppError> {
        Ok(self
            .projects
            .read()
            .await
            .get(&id)
            .filter(|project| {
                project.operator_id == scope.operator_id
                    && project.tenant_id == scope.tenant_id
                    && scope.project_id.is_none_or(|scope_id| scope_id == id)
            })
            .cloned())
    }

    async fn create(&self, scope: &TenantScope, input: ProjectCreate) -> Result<Project, AppError> {
        let id = ProjectId::from(Uuid::new_v4());
        let slug = input
            .slug
            .clone()
            .unwrap_or_else(|| format!("project-{}", &id.to_string()[..8]));
        let project = Project::new(id, scope, slug, input.display_name.clone(), input.settings)?;
        self.insert(project.clone()).await?;
        Ok(project)
    }

    async fn update(
        &self,
        scope: &TenantScope,
        id: ProjectId,
        expected_revision: i64,
        patch: ProjectPatch,
    ) -> Result<UpdateProject, AppError> {
        let mut projects = self.projects.write().await;
        let Some(current) = projects.get(&id) else {
            return Err(AppError::not_found("project not found"));
        };
        if current.operator_id != scope.operator_id
            || current.tenant_id != scope.tenant_id
            || scope.project_id.is_some_and(|scope_id| scope_id != id)
        {
            return Err(AppError::not_found("project not found"));
        }
        if current.revision != expected_revision {
            return Err(AppError::conflict(format!(
                "project revision conflict: expected {expected_revision}, current {}",
                current.revision
            )));
        }
        if let Some(slug) = &patch.slug
            && projects.values().any(|candidate| {
                candidate.id != id
                    && candidate.operator_id == scope.operator_id
                    && candidate.tenant_id == scope.tenant_id
                    && candidate.slug == *slug
            })
        {
            return Err(AppError::conflict(
                "a project with this slug already exists in the tenant",
            ));
        }
        let mut updated = current.clone();
        let previous_revision = updated.revision;
        patch.apply_to(&mut updated)?;
        updated.revision += 1;
        updated.updated_at = Utc::now();
        projects.insert(id, updated.clone());
        Ok(UpdateProject {
            project: updated,
            previous_revision,
        })
    }
}

fn validate_text(field: &str, value: String, max: usize) -> Result<String, AppError> {
    let value = value.trim().to_owned();
    if value.is_empty() {
        return Err(AppError::invalid_request(format!(
            "{field} must not be empty"
        )));
    }
    if value.chars().count() > max {
        return Err(AppError::invalid_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(value)
}

fn default_budget_currency() -> String {
    "CNY".to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProjectOverview {
    pub project: Project,
    pub cycle: OverviewCycle,
    pub knowledge: OverviewKnowledge,
    pub benchmark: OverviewBenchmark,
    pub content: OverviewContent,
    pub cost: OverviewCost,
    pub next_action: Option<OverviewAction>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OverviewCycleStatus {
    NotStarted,
    Running,
    Paused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OverviewCycle {
    pub status: OverviewCycleStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OverviewKnowledgeStatus {
    Empty,
    Importing,
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OverviewKnowledge {
    pub source_count: u64,
    pub fact_count: u64,
    pub status: OverviewKnowledgeStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OverviewBenchmarkStatus {
    NotStarted,
    Running,
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OverviewBenchmark {
    pub question_count: u64,
    pub planned_samples: u64,
    pub effective_samples: Option<u64>,
    pub status: OverviewBenchmarkStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OverviewContent {
    pub published_count: u64,
    pub verified_count: u64,
    pub blocked_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OverviewCost {
    pub currency: String,
    pub reserved_minor: i64,
    pub settled_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OverviewAction {
    pub code: String,
    pub label: String,
    pub href: String,
}

impl ProjectOverview {
    pub fn empty(project: Project) -> Self {
        let currency = project.settings.budget_currency.clone();
        Self {
            updated_at: project.updated_at,
            project,
            cycle: OverviewCycle {
                status: OverviewCycleStatus::NotStarted,
            },
            knowledge: OverviewKnowledge {
                source_count: 0,
                fact_count: 0,
                status: OverviewKnowledgeStatus::Empty,
            },
            benchmark: OverviewBenchmark {
                question_count: 0,
                planned_samples: 0,
                effective_samples: None,
                status: OverviewBenchmarkStatus::NotStarted,
            },
            content: OverviewContent {
                published_count: 0,
                verified_count: 0,
                blocked_count: 0,
            },
            cost: OverviewCost {
                currency,
                reserved_minor: 0,
                settled_minor: 0,
            },
            next_action: Some(OverviewAction {
                code: "import_knowledge".to_owned(),
                label: "导入资料".to_owned(),
                href: "knowledge".to_owned(),
            }),
        }
    }
}
