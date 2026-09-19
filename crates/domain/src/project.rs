use async_trait::async_trait;
use chrono::{DateTime, Datelike, Duration, LocalResult, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::atomic::{AtomicBool, Ordering},
};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum ResourceMode {
    #[default]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum PeriodPolicy {
    #[default]
    PreviousCalendarWeek,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ReportWeekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl ReportWeekday {
    fn num_days_from_monday(self) -> u32 {
        match self {
            Self::Monday => 0,
            Self::Tuesday => 1,
            Self::Wednesday => 2,
            Self::Thursday => 3,
            Self::Friday => 4,
            Self::Saturday => 5,
            Self::Sunday => 6,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReportSchedule {
    #[serde(default = "default_report_weekday")]
    pub report_weekday: ReportWeekday,
    #[serde(default = "default_report_local_time")]
    pub report_local_time: String,
    #[serde(default = "default_cutoff_weekday")]
    pub cutoff_weekday: ReportWeekday,
    #[serde(default = "default_cutoff_local_time")]
    pub cutoff_local_time: String,
    #[serde(default)]
    pub period_policy: PeriodPolicy,
}

impl Default for ReportSchedule {
    fn default() -> Self {
        Self {
            report_weekday: default_report_weekday(),
            report_local_time: default_report_local_time(),
            cutoff_weekday: default_cutoff_weekday(),
            cutoff_local_time: default_cutoff_local_time(),
            period_policy: PeriodPolicy::PreviousCalendarWeek,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuestionClusterState {
    PendingResolution,
    Resolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct QuestionClusterScope {
    pub key: String,
    #[serde(default = "default_question_cluster_state")]
    pub state: QuestionClusterState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DocumentScope {
    #[serde(default = "default_true")]
    pub all_active_products: bool,
    #[serde(default)]
    pub excluded_product_ids: Vec<String>,
    #[serde(default)]
    pub markets: Vec<String>,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub content_types: Vec<String>,
    #[serde(default)]
    pub question_clusters: Vec<QuestionClusterScope>,
}

impl Default for DocumentScope {
    fn default() -> Self {
        Self {
            all_active_products: true,
            excluded_product_ids: Vec::new(),
            markets: Vec::new(),
            languages: Vec::new(),
            content_types: Vec::new(),
            question_clusters: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum DistributionScopeMode {
    #[default]
    AllEligible,
    Explicit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReplicationPolicy {
    #[default]
    OneAccountPerPlatform,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DistributionScope {
    #[serde(default)]
    pub mode: DistributionScopeMode,
    #[serde(default)]
    pub included_platform_ids: Vec<String>,
    #[serde(default)]
    pub excluded_platform_ids: Vec<String>,
    #[serde(default)]
    pub resource_pool_ids: Vec<String>,
    #[serde(default)]
    pub replication_policy: ReplicationPolicy,
}

impl Default for DistributionScope {
    fn default() -> Self {
        Self {
            mode: DistributionScopeMode::AllEligible,
            included_platform_ids: Vec::new(),
            excluded_platform_ids: Vec::new(),
            resource_pool_ids: Vec::new(),
            replication_policy: ReplicationPolicy::OneAccountPerPlatform,
        }
    }
}

/// Editable project configuration.  Draft validation deliberately permits
/// incomplete inputs; only `validate_start` turns this into an executable
/// snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProjectSettings {
    #[serde(default)]
    pub brand_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_name: Option<String>,
    /// Legacy shorthand for one market. It is normalized into document_scope
    /// for start validation and retained for compatibility with W01 clients.
    #[serde(default)]
    pub market: String,
    /// Legacy shorthand for one language.
    #[serde(default)]
    pub language: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_audience: Option<String>,
    #[serde(default)]
    pub competitors: Vec<String>,
    #[serde(default)]
    pub initial_sources: Vec<InitialSource>,
    #[serde(default)]
    pub resource_mode: ResourceMode,
    #[serde(default = "default_budget_currency")]
    pub budget_currency: String,
    #[serde(default)]
    pub monthly_budget_minor: i64,
    #[serde(default = "default_monitoring_reserve_percent")]
    pub monitoring_reserve_percent: u8,
    #[serde(default = "default_report_timezone")]
    pub report_timezone: String,
    #[serde(default)]
    pub report_schedule: ReportSchedule,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    #[serde(default)]
    pub document_scope: DocumentScope,
    #[serde(default)]
    pub distribution_scope: DistributionScope,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            brand_name: String::new(),
            product_name: None,
            market: String::new(),
            language: String::new(),
            target_audience: None,
            competitors: Vec::new(),
            initial_sources: Vec::new(),
            resource_mode: ResourceMode::Own,
            budget_currency: default_budget_currency(),
            monthly_budget_minor: 0,
            monitoring_reserve_percent: default_monitoring_reserve_percent(),
            report_timezone: default_report_timezone(),
            report_schedule: ReportSchedule::default(),
            objective: None,
            document_scope: DocumentScope::default(),
            distribution_scope: DistributionScope::default(),
        }
    }
}

impl ProjectSettings {
    pub fn validate_draft(self) -> Result<Self, AppError> {
        let brand_name = validate_optional_text("brand_name", self.brand_name, 200)?;
        let product_name = self
            .product_name
            .map(|value| validate_text("product_name", value, 200))
            .transpose()?;
        let market = validate_optional_text("market", self.market, 100)?;
        let language = validate_optional_text("language", self.language, 50)?;
        let target_audience = self
            .target_audience
            .map(|value| validate_text("target_audience", value, 200))
            .transpose()?;
        let objective = self
            .objective
            .map(|value| validate_text("objective", value, 2_000))
            .transpose()?;
        if self.competitors.len() > 5 {
            return Err(
                AppError::invalid_request("competitors must contain at most 5 entries")
                    .with_details(json!({ "field": "competitors", "max": 5 })),
            );
        }
        let competitors = self
            .competitors
            .into_iter()
            .map(|value| validate_text("competitors", value, 200))
            .collect::<Result<Vec<_>, _>>()?;
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
        let report_timezone = self.report_timezone.trim().to_owned();
        if report_timezone.parse::<Tz>().is_err() {
            return Err(AppError::invalid_request(
                "report_timezone must be a valid IANA timezone",
            ));
        }
        let report_schedule = validate_report_schedule(self.report_schedule)?;
        let document_scope = validate_document_scope(self.document_scope)?;
        let distribution_scope = validate_distribution_scope(self.distribution_scope)?;
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
            report_timezone,
            report_schedule,
            objective,
            document_scope,
            distribution_scope,
        })
    }

    /// Validate that a draft is complete enough to snapshot and start. A zero
    /// budget is valid: free knowledge work can proceed while paid work remains
    /// blocked by later phases.
    pub fn validate_start(self) -> Result<Self, AppError> {
        let settings = self.validate_draft()?;
        if settings.brand_name.is_empty() {
            return Err(AppError::invalid_request("brand_name is required to start"));
        }
        if settings.initial_sources.is_empty() {
            return Err(AppError::invalid_request(
                "at least one initial source is required to start",
            ));
        }
        if settings.effective_markets().is_empty() || settings.effective_languages().is_empty() {
            return Err(AppError::invalid_request(
                "document_scope must include at least one market and language to start",
            ));
        }
        if !settings.document_scope.all_active_products {
            return Err(AppError::invalid_request(
                "W02 start requires document_scope.all_active_products=true until explicit product inclusion is available",
            ));
        }
        if matches!(
            settings.distribution_scope.mode,
            DistributionScopeMode::Explicit
        ) && settings.distribution_scope.included_platform_ids.is_empty()
        {
            return Err(AppError::invalid_request(
                "explicit distribution_scope requires included_platform_ids",
            ));
        }
        Ok(settings)
    }

    /// Kept as a source-compatible alias for callers that create complete
    /// settings. New callers should choose draft or start validation.
    pub fn validate(self) -> Result<Self, AppError> {
        self.validate_draft()
    }

    pub fn effective_markets(&self) -> Vec<String> {
        if self.document_scope.markets.is_empty() {
            if self.market.is_empty() {
                Vec::new()
            } else {
                vec![self.market.clone()]
            }
        } else {
            self.document_scope.markets.clone()
        }
    }

    pub fn effective_languages(&self) -> Vec<String> {
        if self.document_scope.languages.is_empty() {
            if self.language.is_empty() {
                Vec::new()
            } else {
                vec![self.language.clone()]
            }
        } else {
            self.document_scope.languages.clone()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InitialSourceKind {
    Url,
    Text,
    Object,
    #[serde(alias = "knowledgecollection")]
    KnowledgeCollection,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
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
        let version_ref = self
            .version_ref
            .map(|value| validate_text("initial_sources.version_ref", value, 1_000))
            .transpose()?;
        let content_hash = self.content_hash.map(validate_content_hash).transpose()?;
        // A URL is a mutable locator, never a version snapshot. It can still
        // be an executable source on its own; a later knowledge release must
        // create the immutable content/version reference.
        if matches!(self.kind, InitialSourceKind::Url)
            && version_ref
                .as_deref()
                .is_some_and(|reference| reference == value)
        {
            return Err(AppError::invalid_request(
                "an URL value cannot be used as its own version_ref",
            ));
        }
        Ok(Self {
            value,
            version_ref,
            content_hash,
            ..self
        })
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_config_revision_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_cycle_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_operation_id: Option<Uuid>,
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
            settings: settings.validate_draft()?,
            status: ProjectStatus::Draft,
            revision: 1,
            current_config_revision_id: None,
            current_cycle_id: None,
            start_operation_id: None,
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
    #[serde(skip)]
    pub clear_product_name: bool,
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
    pub report_timezone: Option<String>,
    pub report_schedule: Option<ReportSchedule>,
    pub objective: Option<String>,
    #[serde(skip)]
    pub clear_objective: bool,
    pub document_scope: Option<DocumentScope>,
    pub distribution_scope: Option<DistributionScope>,
    pub status: Option<ProjectStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateProject {
    pub project: Project,
    pub previous_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProjectStartCommand {
    pub expected_revision: i64,
    /// SHA-256 digest of the HTTP idempotency key. The raw client key is never
    /// stored in a start record.
    pub idempotency_key_hash: String,
    /// SHA-256 digest over project/action/revision/settings_hash.
    pub request_hash: String,
    pub settings_hash: String,
    pub operation_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StartAcceptanceStatus {
    Accepted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DocumentManifestAcceptance {
    pub manifest_id: Uuid,
    pub revision: i32,
    pub state: String,
    pub sealed: bool,
    pub expected_count: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DistributionManifestAcceptance {
    pub manifest_id: Uuid,
    pub revision: i32,
    pub state: String,
    pub sealed: bool,
    pub expected_count: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProjectStartAcceptance {
    pub operation_id: Uuid,
    pub cycle_id: Uuid,
    pub config_revision_id: Uuid,
    pub document_manifest: DocumentManifestAcceptance,
    pub distribution_manifest: DistributionManifestAcceptance,
    pub status: StartAcceptanceStatus,
    pub operation_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProjectStartView {
    pub project_id: ProjectId,
    pub acceptance: ProjectStartAcceptance,
    pub requested_revision: i64,
    pub settings_hash: String,
    pub report_window_start_at: DateTime<Utc>,
    pub report_window_end_at: DateTime<Utc>,
    pub cutoff_at: DateTime<Utc>,
}

pub fn settings_hash(settings: &ProjectSettings) -> Result<String, AppError> {
    let bytes = serde_json::to_vec(settings).map_err(|error| {
        AppError::new(
            crate::ErrorCode::Internal,
            format!("project settings cannot be serialized: {error}"),
        )
    })?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

pub fn hash_idempotency_key(key: &str) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}

pub fn start_request_hash(
    project_id: ProjectId,
    expected_revision: i64,
    settings_hash: &str,
) -> String {
    let canonical = format!(
        "project={project_id}\naction=project.start\nrevision={expected_revision}\nsettings_hash={settings_hash}"
    );
    hex::encode(Sha256::digest(canonical.as_bytes()))
}

pub type ReportWindowUtc = (DateTime<Utc>, DateTime<Utc>, DateTime<Utc>);

/// Freeze the next report's prior-calendar-week boundaries into UTC at start
/// time. Repeated local times choose the earlier offset; a local time in a DST
/// gap advances minute by minute to the first valid instant.
pub fn previous_calendar_week_window(
    timezone: &str,
    schedule: &ReportSchedule,
    now: DateTime<Utc>,
) -> Result<ReportWindowUtc, AppError> {
    let timezone = timezone
        .parse::<Tz>()
        .map_err(|_| AppError::invalid_request("report_timezone must be a valid IANA timezone"))?;
    let report_time =
        NaiveTime::parse_from_str(&schedule.report_local_time, "%H:%M").map_err(|_| {
            AppError::invalid_request("report_schedule.report_local_time must be HH:MM")
        })?;
    let cutoff_time =
        NaiveTime::parse_from_str(&schedule.cutoff_local_time, "%H:%M").map_err(|_| {
            AppError::invalid_request("report_schedule.cutoff_local_time must be HH:MM")
        })?;
    let now_local = now.with_timezone(&timezone);
    let mut report_date = now_local.date_naive();
    let report_delta = (7 + schedule.report_weekday.num_days_from_monday() as i64
        - now_local.weekday().num_days_from_monday() as i64)
        % 7;
    report_date += Duration::days(report_delta);
    let mut report_at = resolve_local_datetime(timezone, report_date.and_time(report_time));
    if report_at <= now {
        report_date += Duration::days(7);
        report_at = resolve_local_datetime(timezone, report_date.and_time(report_time));
    }
    let report_week_monday =
        report_date - Duration::days(report_date.weekday().num_days_from_monday() as i64);
    let period_start_date = report_week_monday - Duration::days(7);
    let period_end_date = report_week_monday;
    let mut cutoff_date = report_date;
    let cutoff_delta = (7 + report_date.weekday().num_days_from_monday() as i64
        - schedule.cutoff_weekday.num_days_from_monday() as i64)
        % 7;
    cutoff_date -= Duration::days(cutoff_delta);
    let mut cutoff = resolve_local_datetime(timezone, cutoff_date.and_time(cutoff_time));
    if cutoff > report_at {
        cutoff_date -= Duration::days(7);
        cutoff = resolve_local_datetime(timezone, cutoff_date.and_time(cutoff_time));
    }
    Ok((
        resolve_local_datetime(
            timezone,
            period_start_date
                .and_hms_opt(0, 0, 0)
                .expect("valid midnight"),
        ),
        resolve_local_datetime(
            timezone,
            period_end_date
                .and_hms_opt(0, 0, 0)
                .expect("valid midnight"),
        ),
        cutoff,
    ))
}

fn resolve_local_datetime(timezone: Tz, value: NaiveDateTime) -> DateTime<Utc> {
    let mut candidate = value;
    loop {
        match timezone.from_local_datetime(&candidate) {
            LocalResult::Single(value) => return value.with_timezone(&Utc),
            LocalResult::Ambiguous(earliest, _) => return earliest.with_timezone(&Utc),
            LocalResult::None => candidate += Duration::minutes(1),
        }
    }
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
        if self.clear_product_name {
            settings.product_name = None;
        } else if let Some(value) = &self.product_name {
            settings.product_name = Some(value.clone());
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
        if let Some(value) = &self.report_timezone {
            settings.report_timezone = value.clone();
        }
        if let Some(value) = &self.report_schedule {
            settings.report_schedule = value.clone();
        }
        if self.clear_objective {
            settings.objective = None;
        } else if let Some(value) = &self.objective {
            settings.objective = Some(value.clone());
        }
        if let Some(value) = &self.document_scope {
            settings.document_scope = value.clone();
        }
        if let Some(value) = &self.distribution_scope {
            settings.distribution_scope = value.clone();
        }
        project.settings = settings.validate_draft()?;
        if let Some(status) = self.status
            && status != project.status
        {
            return Err(AppError::invalid_request(
                "project lifecycle transitions must use a command",
            ));
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

    /// Business-level atomic start boundary. PostgreSQL implementations must
    /// make the complete skeleton durable in one transaction; the memory
    /// implementation holds one state lock.
    async fn start(
        &self,
        _scope: &TenantScope,
        _id: ProjectId,
        _command: ProjectStartCommand,
    ) -> Result<ProjectStartAcceptance, AppError> {
        Err(AppError::new(
            crate::ErrorCode::DependencyUnavailable,
            "project repository does not support atomic start",
        ))
    }

    async fn get_start(
        &self,
        _scope: &TenantScope,
        _id: ProjectId,
    ) -> Result<Option<ProjectStartView>, AppError> {
        Ok(None)
    }
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
struct MemoryProjectState {
    projects: HashMap<ProjectId, Project>,
    starts: HashMap<ProjectId, MemoryStartRecord>,
}

#[derive(Debug, Clone)]
struct MemoryStartRecord {
    idempotency_key_hash: String,
    request_hash: String,
    view: ProjectStartView,
}

#[derive(Debug, Default)]
pub struct MemoryProjectRepository {
    state: RwLock<MemoryProjectState>,
    fail_next_start: AtomicBool,
}

impl MemoryProjectRepository {
    pub fn new() -> Self {
        Self::default()
    }

    /// Test-only fault injection for the atomic boundary. The failure is
    /// observed before any project/start state mutation, so callers can prove
    /// all-or-nothing behavior without a database.
    pub fn fail_next_start_for_test(&self) {
        self.fail_next_start.store(true, Ordering::Release);
    }

    pub async fn insert(&self, project: Project) -> Result<(), AppError> {
        let mut state = self.state.write().await;
        if state.projects.values().any(|candidate| {
            candidate.operator_id == project.operator_id
                && candidate.tenant_id == project.tenant_id
                && candidate.slug == project.slug
                && candidate.id != project.id
        }) {
            return Err(AppError::conflict(
                "a project with this slug already exists in the tenant",
            ));
        }
        state.projects.insert(project.id, project);
        Ok(())
    }
}

#[async_trait]
impl ProjectRepository for MemoryProjectRepository {
    async fn list(&self, scope: &TenantScope) -> Result<Vec<Project>, AppError> {
        let mut result = self
            .state
            .read()
            .await
            .projects
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
            .state
            .read()
            .await
            .projects
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
        let mut state = self.state.write().await;
        let Some(current) = state.projects.get(&id) else {
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
            && state.projects.values().any(|candidate| {
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
        state.projects.insert(id, updated.clone());
        Ok(UpdateProject {
            project: updated,
            previous_revision,
        })
    }

    async fn start(
        &self,
        scope: &TenantScope,
        id: ProjectId,
        command: ProjectStartCommand,
    ) -> Result<ProjectStartAcceptance, AppError> {
        let mut state = self.state.write().await;
        let current = state
            .projects
            .get(&id)
            .cloned()
            .ok_or_else(|| AppError::not_found("project not found"))?;
        if current.operator_id != scope.operator_id
            || current.tenant_id != scope.tenant_id
            || scope.project_id.is_some_and(|scope_id| scope_id != id)
        {
            return Err(AppError::not_found("project not found"));
        }
        if let Some(record) = state.starts.get(&id) {
            if record.idempotency_key_hash == command.idempotency_key_hash {
                if record.request_hash == command.request_hash {
                    return Ok(record.view.acceptance.clone());
                }
                return Err(AppError::conflict(
                    "Idempotency-Key was already used with a different project start request",
                ));
            }
            return Err(AppError::conflict("project has already been started"));
        }
        if current.status != ProjectStatus::Draft {
            return Err(AppError::conflict("project has already been started"));
        }
        if current.revision != command.expected_revision {
            return Err(AppError::conflict(format!(
                "project revision conflict: expected {}, current {}",
                command.expected_revision, current.revision
            )));
        }
        let settings = current.settings.clone().validate_start()?;
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
        let acceptance = ProjectStartAcceptance {
            operation_id: command.operation_id,
            cycle_id,
            config_revision_id,
            document_manifest: DocumentManifestAcceptance {
                manifest_id: document_manifest_id,
                revision: 1,
                state: "awaiting_knowledge".to_owned(),
                sealed: false,
                expected_count: None,
            },
            distribution_manifest: DistributionManifestAcceptance {
                manifest_id: distribution_manifest_id,
                revision: 1,
                state: "awaiting_documents".to_owned(),
                sealed: false,
                expected_count: None,
            },
            status: StartAcceptanceStatus::Accepted,
            operation_url: format!("/api/v1/operations/{}", command.operation_id),
        };
        if self.fail_next_start.swap(false, Ordering::AcqRel) {
            return Err(AppError::new(
                crate::ErrorCode::DependencyUnavailable,
                "injected project start failure",
            ));
        }
        let view = ProjectStartView {
            project_id: id,
            acceptance: acceptance.clone(),
            requested_revision: command.expected_revision,
            settings_hash: computed_settings_hash,
            report_window_start_at,
            report_window_end_at,
            cutoff_at,
        };
        let mut updated = current;
        updated.status = ProjectStatus::Active;
        updated.revision += 1;
        updated.updated_at = Utc::now();
        updated.current_config_revision_id = Some(config_revision_id);
        updated.current_cycle_id = Some(cycle_id);
        updated.start_operation_id = Some(command.operation_id);
        state.projects.insert(id, updated);
        state.starts.insert(
            id,
            MemoryStartRecord {
                idempotency_key_hash: command.idempotency_key_hash,
                request_hash: command.request_hash,
                view,
            },
        );
        Ok(acceptance)
    }

    async fn get_start(
        &self,
        scope: &TenantScope,
        id: ProjectId,
    ) -> Result<Option<ProjectStartView>, AppError> {
        let state = self.state.read().await;
        let Some(project) = state.projects.get(&id) else {
            return Ok(None);
        };
        if project.operator_id != scope.operator_id
            || project.tenant_id != scope.tenant_id
            || scope.project_id.is_some_and(|scope_id| scope_id != id)
        {
            return Ok(None);
        }
        Ok(state.starts.get(&id).map(|record| record.view.clone()))
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

fn validate_optional_text(field: &str, value: String, max: usize) -> Result<String, AppError> {
    let value = value.trim().to_owned();
    if value.chars().count() > max {
        return Err(AppError::invalid_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(value)
}

fn validate_content_hash(value: String) -> Result<String, AppError> {
    let value = value.trim().to_ascii_lowercase();
    if value.len() < 16 || value.len() > 256 || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(AppError::invalid_request(
            "initial_sources.content_hash must be a hexadecimal content digest",
        ));
    }
    Ok(value)
}

fn validate_report_schedule(schedule: ReportSchedule) -> Result<ReportSchedule, AppError> {
    let report_local_time = validate_time(
        "report_schedule.report_local_time",
        schedule.report_local_time,
    )?;
    let cutoff_local_time = validate_time(
        "report_schedule.cutoff_local_time",
        schedule.cutoff_local_time,
    )?;
    Ok(ReportSchedule {
        report_weekday: schedule.report_weekday,
        report_local_time,
        cutoff_weekday: schedule.cutoff_weekday,
        cutoff_local_time,
        period_policy: schedule.period_policy,
    })
}

fn validate_time(field: &str, value: String) -> Result<String, AppError> {
    let value = value.trim().to_owned();
    NaiveTime::parse_from_str(&value, "%H:%M")
        .map_err(|_| AppError::invalid_request(format!("{field} must be HH:MM")))?;
    Ok(value)
}

fn validate_scope_values(
    field: &str,
    values: Vec<String>,
    max: usize,
) -> Result<Vec<String>, AppError> {
    if values.len() > max {
        return Err(AppError::invalid_request(format!(
            "{field} has too many entries"
        )));
    }
    values
        .into_iter()
        .map(|value| validate_text(field, value, 200))
        .collect()
}

fn validate_document_scope(scope: DocumentScope) -> Result<DocumentScope, AppError> {
    if scope.all_active_products && !scope.excluded_product_ids.is_empty() {
        // Exclusions are intentional under the all-active default.
    }
    if scope.question_clusters.len() > 100 {
        return Err(AppError::invalid_request(
            "document_scope.question_clusters has too many entries",
        ));
    }
    let question_clusters = scope
        .question_clusters
        .into_iter()
        .map(|cluster| {
            Ok(QuestionClusterScope {
                key: validate_text("document_scope.question_clusters.key", cluster.key, 200)?,
                state: cluster.state,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(DocumentScope {
        all_active_products: scope.all_active_products,
        excluded_product_ids: validate_scope_values(
            "document_scope.excluded_product_ids",
            scope.excluded_product_ids,
            10_000,
        )?,
        markets: validate_scope_values("document_scope.markets", scope.markets, 100)?,
        languages: validate_scope_values("document_scope.languages", scope.languages, 100)?,
        content_types: validate_scope_values(
            "document_scope.content_types",
            scope.content_types,
            100,
        )?,
        question_clusters,
    })
}

fn validate_distribution_scope(scope: DistributionScope) -> Result<DistributionScope, AppError> {
    Ok(DistributionScope {
        mode: scope.mode,
        included_platform_ids: validate_scope_values(
            "distribution_scope.included_platform_ids",
            scope.included_platform_ids,
            1_000,
        )?,
        excluded_platform_ids: validate_scope_values(
            "distribution_scope.excluded_platform_ids",
            scope.excluded_platform_ids,
            1_000,
        )?,
        resource_pool_ids: validate_scope_values(
            "distribution_scope.resource_pool_ids",
            scope.resource_pool_ids,
            1_000,
        )?,
        replication_policy: scope.replication_policy,
    })
}

fn default_budget_currency() -> String {
    "CNY".to_owned()
}

fn default_monitoring_reserve_percent() -> u8 {
    20
}

fn default_report_timezone() -> String {
    "Asia/Shanghai".to_owned()
}

fn default_report_weekday() -> ReportWeekday {
    ReportWeekday::Monday
}

fn default_report_local_time() -> String {
    "09:00".to_owned()
}

fn default_cutoff_weekday() -> ReportWeekday {
    ReportWeekday::Monday
}

fn default_cutoff_local_time() -> String {
    "00:00".to_owned()
}

fn default_question_cluster_state() -> QuestionClusterState {
    QuestionClusterState::PendingResolution
}

fn default_true() -> bool {
    true
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
    /// W02 has created only the cycle skeleton. Knowledge-derived work has
    /// not begun and must never be presented as running.
    #[serde(default)]
    pub awaiting_knowledge: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle_id: Option<Uuid>,
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
                awaiting_knowledge: false,
                cycle_id: None,
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

    pub fn from_start(project: Project, start: Option<ProjectStartView>) -> Self {
        let mut overview = Self::empty(project);
        if let Some(start) = start {
            overview.cycle = OverviewCycle {
                status: OverviewCycleStatus::NotStarted,
                awaiting_knowledge: true,
                cycle_id: Some(start.acceptance.cycle_id),
            };
        }
        overview
    }
}

#[cfg(test)]
mod tests {
    use super::{
        InitialSource, InitialSourceKind, InitialSourceVisibility, MemoryProjectRepository,
        ProjectCreate, ProjectRepository, ProjectSettings, ProjectStartCommand, ProjectStatus,
        ReportSchedule, ReportWeekday, hash_idempotency_key, previous_calendar_week_window,
        settings_hash, start_request_hash,
    };
    use chrono::{DateTime, Utc};
    use uuid::Uuid;

    fn utc(value: &str) -> DateTime<Utc> {
        value.parse::<DateTime<Utc>>().expect("valid UTC fixture")
    }

    #[test]
    fn next_monday_report_uses_the_immediately_previous_calendar_week() {
        let schedule = ReportSchedule {
            report_weekday: ReportWeekday::Monday,
            report_local_time: "09:00".to_owned(),
            cutoff_weekday: ReportWeekday::Sunday,
            cutoff_local_time: "23:59".to_owned(),
            ..ReportSchedule::default()
        };
        let (start, end, cutoff) =
            previous_calendar_week_window("Asia/Shanghai", &schedule, utc("2026-01-05T00:00:00Z"))
                .expect("window");
        assert_eq!(start, utc("2025-12-28T16:00:00Z"));
        assert_eq!(end, utc("2026-01-04T16:00:00Z"));
        assert_eq!(cutoff, utc("2026-01-04T15:59:00Z"));
    }

    #[test]
    fn dst_gap_advances_to_first_valid_local_minute() {
        let schedule = ReportSchedule {
            report_weekday: ReportWeekday::Monday,
            report_local_time: "09:00".to_owned(),
            cutoff_weekday: ReportWeekday::Sunday,
            cutoff_local_time: "02:30".to_owned(),
            ..ReportSchedule::default()
        };
        let (start, end, cutoff) = previous_calendar_week_window(
            "America/New_York",
            &schedule,
            utc("2026-03-07T12:00:00Z"),
        )
        .expect("window");
        assert_eq!(start, utc("2026-03-02T05:00:00Z"));
        assert_eq!(end, utc("2026-03-09T04:00:00Z"));
        assert_eq!(cutoff, utc("2026-03-08T07:00:00Z"));
    }

    #[tokio::test]
    async fn injected_memory_start_failure_leaves_no_partial_start_state() {
        let repository = MemoryProjectRepository::default();
        let scope = super::TenantScope::new(Uuid::new_v4().into(), Uuid::new_v4().into(), None);
        let project = repository
            .create(
                &scope,
                ProjectCreate {
                    slug: Some("atomic-fault".to_owned()),
                    display_name: "Atomic fault".to_owned(),
                    settings: ProjectSettings {
                        brand_name: "Acme".to_owned(),
                        market: "US".to_owned(),
                        language: "en".to_owned(),
                        initial_sources: vec![InitialSource {
                            kind: InitialSourceKind::Url,
                            value: "https://example.com".to_owned(),
                            visibility: InitialSourceVisibility::Public,
                            version_ref: None,
                            content_hash: None,
                        }],
                        ..ProjectSettings::default()
                    },
                },
            )
            .await
            .expect("draft");
        let frozen = project
            .settings
            .clone()
            .validate_start()
            .expect("startable");
        let frozen_hash = settings_hash(&frozen).expect("hash");
        let command = ProjectStartCommand {
            expected_revision: project.revision,
            idempotency_key_hash: hash_idempotency_key("fault-key"),
            request_hash: start_request_hash(project.id, project.revision, &frozen_hash),
            settings_hash: frozen_hash,
            operation_id: Uuid::new_v4(),
        };
        repository.fail_next_start_for_test();
        assert!(
            repository
                .start(&scope, project.id, command.clone())
                .await
                .is_err()
        );
        let after_failure = repository
            .get(&scope, project.id)
            .await
            .expect("project")
            .expect("visible");
        assert_eq!(after_failure.status, ProjectStatus::Draft);
        assert!(after_failure.current_cycle_id.is_none());
        assert!(
            repository
                .get_start(&scope, project.id)
                .await
                .expect("start lookup")
                .is_none()
        );
        assert!(repository.start(&scope, project.id, command).await.is_ok());
    }
}
