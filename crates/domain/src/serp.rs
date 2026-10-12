//! Independent search-result observations. Provider adapters retain raw evidence;
//! this module never derives organic rank from advertisements or AI citations.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use url::Url;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{AppError, FrozenQuestionBinding, TenantScope, sha256_hex};

pub const SERP_PROTOCOL_VERSION: &str = "geo.serp.v1";
pub const SERP_URL_RULE_VERSION: &str = "geo.serp.url.v1";
pub const SERP_TARGET_RULE_VERSION: &str = "geo.serp.target.v1";
pub const MAX_SERP_RESULTS: usize = 200;
pub const MAX_SERP_RAW_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SerpEngine {
    Google,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SerpSurface {
    ThirdPartyApi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SerpDevice {
    Desktop,
}

/// Requested conditions only. An echoed request is NOT an actual-condition proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SerpProtocol {
    pub query: String,
    pub engine: SerpEngine,
    pub surface: SerpSurface,
    pub source: String,
    /// Exact source-specific geographic selector, not a display label.
    pub source_location_code: String,
    pub country: String,
    pub city: Option<String>,
    pub language: String,
    pub device: SerpDevice,
    pub operating_system: String,
    pub requested_depth: u32,
    pub max_pages: u32,
    pub priority: u32,
    pub login: String,
    pub personalization: String,
    pub protocol_version: String,
    pub connector_version: String,
}

fn label(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn invalid() -> AppError {
    AppError::invalid_request("invalid search observation contract")
}

impl SerpProtocol {
    pub fn validate(&self) -> Result<(), AppError> {
        if !label(&self.query, 2800)
            || self.query.chars().count() > 700
            || !label(&self.source, 128)
            || !label(&self.source_location_code, 128)
            || !label(&self.country, 128)
            || !label(&self.language, 64)
            || self.city.as_ref().is_some_and(|v| !label(v, 256))
            || self.operating_system != "windows"
            || self.requested_depth != 10
            || self.max_pages != 1
            || self.priority != 1
            || self.login != "unspecified"
            || self.personalization != "unspecified"
            || self.protocol_version != SERP_PROTOCOL_VERSION
            || !label(&self.connector_version, 128)
        {
            return Err(invalid());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SerpTarget {
    Url {
        url: String,
    },
    Host {
        host: String,
        include_subdomains: bool,
    },
}

/// WHATWG URL handling comes from the maintained `url` crate. This version drops
/// fragments but preserves scheme, path, query order, www, and trailing slashes.
pub fn normalize_serp_url(raw: &str) -> Result<(String, String), AppError> {
    if raw.len() > 8192 || raw.trim() != raw || raw.chars().any(char::is_control) {
        return Err(invalid());
    }
    let mut url = Url::parse(raw).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(invalid());
    }
    let host = url.host_str().ok_or_else(invalid)?.to_owned();
    url.set_fragment(None);
    Ok((url.to_string(), host))
}

impl SerpTarget {
    pub fn normalized(&self) -> Result<Self, AppError> {
        match self {
            Self::Url { url } => Ok(Self::Url {
                url: normalize_serp_url(url)?.0,
            }),
            Self::Host {
                host,
                include_subdomains,
            } => {
                if !label(host, 253)
                    || host.contains(['/', '\\', '?', '#', '@', ':'])
                    || host.ends_with('.')
                {
                    return Err(invalid());
                }
                let (_, normalized) = normalize_serp_url(&format!("https://{host}/"))?;
                Ok(Self::Host {
                    host: normalized,
                    include_subdomains: *include_subdomains,
                })
            }
        }
    }

    pub fn matches(&self, result: &SerpResult) -> Result<bool, AppError> {
        result.validate()?;
        Ok(match self.normalized()? {
            Self::Url { url } => result.normalized_url.as_ref() == Some(&url),
            Self::Host {
                host,
                include_subdomains,
            } => result.host.as_ref().is_some_and(|result_host| {
                *result_host == host
                    || (include_subdomains && result_host.ends_with(&format!(".{host}")))
            }),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SerpMeasurement {
    pub measurement_id: Uuid,
    /// Frozen opaque server-side route identity. Equal sampling protocols do not
    /// authorize substituting another credential/account configuration.
    pub source_key: String,
    pub protocol: SerpProtocol,
    pub target: Option<SerpTarget>,
    pub target_rule_version: String,
    pub question_binding: Option<FrozenQuestionBinding>,
    pub scheduled_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub state: SerpTaskState,
}

impl SerpMeasurement {
    pub fn validate(&self, scope: &TenantScope) -> Result<(), AppError> {
        if scope.project_id.is_none() {
            return Err(AppError::forbidden("project scope required"));
        }
        self.protocol.validate()?;
        if self.measurement_id.is_nil()
            || !label(&self.source_key, 128)
            || self.target_rule_version != SERP_TARGET_RULE_VERSION
        {
            return Err(invalid());
        }
        if let Some(target) = &self.target {
            target.normalized()?;
        }
        if self.question_binding.as_ref().is_some_and(|binding| {
            let reference = binding.reference;
            reference.question_set_id.is_nil()
                || reference.question_set_version_id.is_nil()
                || reference.question_id.is_nil()
                || reference.question_revision_id.is_nil()
                || !label(&binding.split_policy_version, 128)
        }) {
            return Err(invalid());
        }
        Ok(())
    }

    pub fn validate_accept(
        &self,
        scope: &TenantScope,
        idempotency_key: &str,
    ) -> Result<(), AppError> {
        self.validate(scope)?;
        if self.state != SerpTaskState::Queued || !label(idempotency_key, 256) {
            return Err(invalid());
        }
        Ok(())
    }

    /// Generated IDs/creation times do not change input; query/schedule bytes do.
    pub fn same_input(&self, other: &Self) -> bool {
        self.source_key == other.source_key
            && self.protocol == other.protocol
            && self.target == other.target
            && self.target_rule_version == other.target_rule_version
            && self.question_binding == other.question_binding
            && self.scheduled_at == other.scheduled_at
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SerpResultKind {
    Organic,
    Advertisement,
    FeaturedSnippet,
    Maps,
    AiOverview,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SerpResult {
    pub kind: SerpResultKind,
    pub raw_kind: String,
    pub raw_url: Option<String>,
    pub normalized_url: Option<String>,
    pub host: Option<String>,
    pub normalization_version: String,
    pub title: Option<String>,
    pub page: Option<u32>,
    /// Position within the provider's returned result list, not organic rank.
    pub position: u32,
    pub organic_rank: Option<u32>,
    pub absolute_position: Option<u32>,
    /// JSON pointer into the exact immutable raw response.
    pub locator: String,
}

impl SerpResult {
    pub fn validate(&self) -> Result<(), AppError> {
        let normalized = self
            .raw_url
            .as_deref()
            .and_then(|raw| normalize_serp_url(raw).ok());
        if normalized.as_ref().map(|value| &value.0) != self.normalized_url.as_ref()
            || normalized.as_ref().map(|value| &value.1) != self.host.as_ref()
            || self
                .raw_url
                .as_ref()
                .is_some_and(|raw| raw.len() > 8192 || raw.chars().any(char::is_control))
            || self.normalization_version != SERP_URL_RULE_VERSION
            || !label(&self.raw_kind, 128)
            || self.title.as_ref().is_some_and(|title| title.len() > 8192)
            || self.page == Some(0)
            || self.position == 0
            || self.organic_rank == Some(0)
            || self.absolute_position == Some(0)
            || (self.kind != SerpResultKind::Organic && self.organic_rank.is_some())
            || !label(&self.locator, 1024)
            || !self.locator.starts_with('/')
        {
            return Err(invalid());
        }
        Ok(())
    }
}

/// Independently reported effective condition and its raw-evidence locator.
/// Do not populate this from request echoes, defaults, or requested parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SerpActualCondition {
    pub value: String,
    pub evidence_locator: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SerpActualConditions {
    pub country: Option<SerpActualCondition>,
    pub city: Option<SerpActualCondition>,
    pub language: Option<SerpActualCondition>,
    pub device: Option<SerpActualCondition>,
    pub login: Option<SerpActualCondition>,
    pub personalization: Option<SerpActualCondition>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SerpCoverageCompletion {
    RequestedDepth,
    ProviderExhausted,
    Partial,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SerpCoverage {
    pub requested_depth: u32,
    pub observed_organic_depth: u32,
    pub pages_received: u32,
    pub completion: SerpCoverageCompletion,
    pub truncated: bool,
    /// Required for an explicit provider assertion that no deeper results exist.
    pub exhaustion_evidence_locator: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SerpObservationStatus {
    Observed,
    Partial,
    Challenge,
    LoginRequired,
    Missing,
    Failed,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SerpLimitationCode {
    RequestedConditionsUnverified,
    LoginUnknown,
    PersonalizationUnknown,
    SinglePageLimit,
    IncompleteBody,
    ProviderTruncation,
    MissingPositions,
    InvalidResultUrl,
    ProviderReportedTime,
    ResultUnavailable,
    UnsupportedConditions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SerpTargetMatch {
    NotRequested,
    Hit { organic_ranks: Vec<u32> },
    NotFoundWithinDepth { covered_depth: u32 },
    Undetermined,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SerpObservation {
    pub observation_id: Uuid,
    pub measurement_id: Uuid,
    pub attempt_id: Uuid,
    pub raw_evidence_id: Uuid,
    pub raw_sha256: String,
    pub parser_version: String,
    /// Supplier-reported observation time, never synthesized from receipt time.
    pub provider_observed_at: Option<DateTime<Utc>>,
    pub received_at: DateTime<Utc>,
    pub analyzed_at: DateTime<Utc>,
    pub status: SerpObservationStatus,
    pub actual_conditions: SerpActualConditions,
    pub coverage: SerpCoverage,
    pub results: Vec<SerpResult>,
    pub source_limitations: Vec<SerpLimitationCode>,
}

impl SerpObservation {
    /// Validate receipt binding, not application/database clock ordering. Persistence
    /// must load the scoped, already stored raw evidence before appending analysis.
    pub fn validate_source(
        &self,
        measurement: &SerpMeasurement,
        raw: &SerpStoredRaw,
    ) -> Result<(), AppError> {
        self.validate(measurement)?;
        if self.raw_evidence_id != raw.evidence.evidence_id
            || self.raw_sha256 != raw.evidence.response_sha256
            || self.measurement_id != raw.evidence.measurement_id
            || self.attempt_id != raw.evidence.attempt_id
            || self.received_at != raw.evidence.captured_at
            || (!raw.evidence.body_complete && self.status == SerpObservationStatus::Observed)
            || (raw.evidence.operation == SerpEvidenceOperation::Submission
                && (self.status == SerpObservationStatus::Observed || !self.results.is_empty()))
        {
            return Err(invalid());
        }
        Ok(())
    }

    pub fn validate(&self, measurement: &SerpMeasurement) -> Result<(), AppError> {
        measurement.protocol.validate()?;
        if let Some(target) = &measurement.target {
            target.normalized()?;
        }
        if measurement.measurement_id.is_nil()
            || measurement.target_rule_version != SERP_TARGET_RULE_VERSION
            || self.observation_id.is_nil()
            || self.attempt_id.is_nil()
            || self.raw_evidence_id.is_nil()
            || self.measurement_id != measurement.measurement_id
            || !digest(&self.raw_sha256)
            || !label(&self.parser_version, 128)
            || self.received_at > self.analyzed_at
            || self
                .provider_observed_at
                .is_some_and(|at| at > self.received_at)
            || self.results.len() > MAX_SERP_RESULTS
            || self.coverage.requested_depth != measurement.protocol.requested_depth
            || self.coverage.pages_received > measurement.protocol.max_pages
            || self.source_limitations.len() > 32
        {
            return Err(invalid());
        }
        let mut ranks = std::collections::BTreeSet::new();
        let mut matchable_ranks = std::collections::BTreeSet::new();
        let mut positions = std::collections::BTreeSet::new();
        for result in &self.results {
            result.validate()?;
            if result
                .page
                .is_some_and(|page| page > self.coverage.pages_received)
                || !positions.insert(result.position)
                || result.organic_rank.is_some_and(|rank| !ranks.insert(rank))
            {
                return Err(invalid());
            }
            if result.normalized_url.is_some()
                && let Some(rank) = result.organic_rank
            {
                matchable_ranks.insert(rank);
            }
        }
        let contiguous_depth = (1..)
            .take_while(|rank| matchable_ranks.contains(rank))
            .count() as u32;
        if self.coverage.observed_organic_depth != contiguous_depth {
            return Err(invalid());
        }
        match self.coverage.completion {
            SerpCoverageCompletion::RequestedDepth
                if contiguous_depth < self.coverage.requested_depth =>
            {
                return Err(invalid());
            }
            SerpCoverageCompletion::ProviderExhausted
                if !self
                    .coverage
                    .exhaustion_evidence_locator
                    .as_ref()
                    .is_some_and(|v| label(v, 1024) && v.starts_with('/')) =>
            {
                return Err(invalid());
            }
            _ => {}
        }
        let complete = self.coverage.completion != SerpCoverageCompletion::Partial;
        if (complete && self.coverage.truncated)
            || (complete
                && (ranks.len() != contiguous_depth as usize
                    || self.results.iter().any(|result| {
                        result.kind == SerpResultKind::Organic
                            && (result.organic_rank.is_none() || result.normalized_url.is_none())
                    })))
            || (self.status == SerpObservationStatus::Observed && !complete)
            || (self.status == SerpObservationStatus::Partial && complete)
            || (!matches!(
                self.status,
                SerpObservationStatus::Observed | SerpObservationStatus::Partial
            ) && (!self.results.is_empty() || complete))
        {
            return Err(invalid());
        }
        for condition in [
            &self.actual_conditions.country,
            &self.actual_conditions.city,
            &self.actual_conditions.language,
            &self.actual_conditions.device,
            &self.actual_conditions.login,
            &self.actual_conditions.personalization,
        ]
        .into_iter()
        .flatten()
        {
            if !label(&condition.value, 256)
                || !label(&condition.evidence_locator, 1024)
                || !condition.evidence_locator.starts_with('/')
            {
                return Err(invalid());
            }
        }
        Ok(())
    }

    pub fn target_match(&self, measurement: &SerpMeasurement) -> Result<SerpTargetMatch, AppError> {
        self.validate(measurement)?;
        let Some(target) = &measurement.target else {
            return Ok(SerpTargetMatch::NotRequested);
        };
        let mut ranks = Vec::new();
        for result in &self.results {
            if result.kind == SerpResultKind::Organic
                && target.matches(result)?
                && let Some(rank) = result.organic_rank
            {
                ranks.push(rank);
            }
        }
        ranks.sort_unstable();
        if !ranks.is_empty() {
            return Ok(SerpTargetMatch::Hit {
                organic_ranks: ranks,
            });
        }
        if self.status == SerpObservationStatus::Observed {
            Ok(SerpTargetMatch::NotFoundWithinDepth {
                covered_depth: self.coverage.observed_organic_depth,
            })
        } else {
            Ok(SerpTargetMatch::Undetermined)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SerpComparison {
    /// Same frozen sampling protocol and matching rules. Does not assert that
    /// requested geography/device/personalization were actually honored.
    pub protocol_compatible: bool,
    pub actual_conditions_verified: bool,
}

/// Protocol-comparable trends remain useful with unknown actual conditions;
/// callers retain that uncertainty instead of inventing verified conditions.
pub fn serp_samples_comparable(
    left: &SerpMeasurement,
    left_observation: &SerpObservation,
    right: &SerpMeasurement,
    right_observation: &SerpObservation,
) -> bool {
    compare_serp_samples(left, left_observation, right, right_observation).protocol_compatible
}

pub fn compare_serp_samples(
    left: &SerpMeasurement,
    left_observation: &SerpObservation,
    right: &SerpMeasurement,
    right_observation: &SerpObservation,
) -> SerpComparison {
    let conditions = &left_observation.actual_conditions;
    let pairs = [
        (
            &conditions.country,
            &right_observation.actual_conditions.country,
        ),
        (&conditions.city, &right_observation.actual_conditions.city),
        (
            &conditions.language,
            &right_observation.actual_conditions.language,
        ),
        (
            &conditions.device,
            &right_observation.actual_conditions.device,
        ),
        (
            &conditions.login,
            &right_observation.actual_conditions.login,
        ),
        (
            &conditions.personalization,
            &right_observation.actual_conditions.personalization,
        ),
    ];
    let protocol_compatible = left.protocol == right.protocol
        && left.target_rule_version == right.target_rule_version
        && left
            .target
            .as_ref()
            .map(SerpTarget::normalized)
            .transpose()
            .ok()
            == right
                .target
                .as_ref()
                .map(SerpTarget::normalized)
                .transpose()
                .ok()
        && left_observation.validate(left).is_ok()
        && right_observation.validate(right).is_ok()
        && pairs.iter().all(|(left, right)| match (left, right) {
            (Some(left), Some(right)) => left.value == right.value,
            _ => true,
        });
    let actual_conditions_verified = protocol_compatible
        && conditions.country.is_some()
        && conditions.language.is_some()
        && conditions.device.is_some()
        && conditions.login.is_some()
        && conditions.personalization.is_some()
        && (left.protocol.city.is_none() || conditions.city.is_some())
        && [
            (
                &conditions.country,
                &right_observation.actual_conditions.country,
            ),
            (&conditions.city, &right_observation.actual_conditions.city),
            (
                &conditions.language,
                &right_observation.actual_conditions.language,
            ),
            (
                &conditions.device,
                &right_observation.actual_conditions.device,
            ),
            (
                &conditions.login,
                &right_observation.actual_conditions.login,
            ),
            (
                &conditions.personalization,
                &right_observation.actual_conditions.personalization,
            ),
        ]
        .iter()
        .all(|(left, right)| left.as_ref().map(|v| &v.value) == right.as_ref().map(|v| &v.value));
    SerpComparison {
        protocol_compatible,
        actual_conditions_verified,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SerpTaskState {
    Queued,
    Claimed,
    Sending,
    AwaitingResult,
    Completed,
    Unknown,
    Failed,
    Cancelled,
}

pub fn validate_serp_task_transition(
    from: SerpTaskState,
    to: SerpTaskState,
) -> Result<(), AppError> {
    use SerpTaskState::*;
    if matches!(
        (from, to),
        (Queued, Claimed | Cancelled)
            | (Unknown, Cancelled)
            | (Claimed, Sending | Failed | Cancelled)
            | (
                Sending,
                AwaitingResult | Completed | Unknown | Failed | Cancelled
            )
            | (AwaitingResult, Completed | Unknown | Failed | Cancelled)
    ) {
        Ok(())
    } else {
        Err(AppError::conflict("invalid search task transition"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerpClaim {
    pub measurement: SerpMeasurement,
    pub attempt_id: Uuid,
    pub claim_token: Uuid,
    pub claimed_at: DateTime<Utc>,
    pub lease_expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerpSendingIntent {
    pub measurement_id: Uuid,
    pub attempt_id: Uuid,
    pub send_token: Uuid,
    pub request_sha256: String,
    pub correlation_tag: String,
    /// Exact project credential version used for this submission. Legacy
    /// absence does not authorize guessing a current account for task reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_revision: Option<i64>,
    pub intended_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SerpSendCertainty {
    NotSent,
    MayHaveBeenSent,
    ResponseReceived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SerpEvidenceOperation {
    Submission,
    ResultRead,
    RecoveryRead,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerpProviderTask {
    pub measurement_id: Uuid,
    pub attempt_id: Uuid,
    pub binding_evidence_id: Uuid,
    pub provider_task_id: String,
    pub correlation_tag: String,
}

/// Private raw bytes belong in access-controlled storage, not user-visible logs.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerpRawEvidence {
    pub evidence_id: Uuid,
    pub measurement_id: Uuid,
    pub attempt_id: Uuid,
    pub operation: SerpEvidenceOperation,
    /// None for submission (raw is stored before decoding its new task ID).
    /// Reads always name one exact external task, never an account-wide discovery.
    pub provider_task_id: Option<String>,
    /// Exact request for this operation. Result reads have their own digest.
    pub request_sha256: String,
    /// Immutable original submission request binding, including on result reads.
    pub intent_request_sha256: String,
    pub response_sha256: String,
    pub body: Vec<u8>,
    pub body_complete: bool,
    pub http_status: Option<u16>,
    pub send_certainty: SerpSendCertainty,
    pub captured_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerpStoredRaw {
    pub evidence: SerpRawEvidence,
    /// Authoritative repository clock, retained unchanged on idempotent replay.
    pub stored_at: DateTime<Utc>,
}

/// Internal recovery state, not an HTTP response: the intent contains a token
/// authorizing immutable evidence append, never another provider submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerpExecution {
    pub measurement_id: Uuid,
    pub claim: Option<SerpClaim>,
    pub intent: Option<SerpSendingIntent>,
    pub provider_task: Option<SerpProviderTask>,
    pub next_poll_at: Option<DateTime<Utc>>,
}

/// Scoped evidence inventory without response bytes or execution tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SerpRawReceipt {
    pub evidence_id: Uuid,
    pub measurement_id: Uuid,
    pub attempt_id: Uuid,
    pub operation: SerpEvidenceOperation,
    pub provider_task_id: Option<String>,
    pub request_sha256: String,
    pub intent_request_sha256: String,
    pub response_sha256: String,
    pub body_bytes: usize,
    pub body_complete: bool,
    pub http_status: Option<u16>,
    pub send_certainty: SerpSendCertainty,
    pub captured_at: DateTime<Utc>,
    pub stored_at: DateTime<Utc>,
}

impl SerpStoredRaw {
    pub fn receipt(&self) -> SerpRawReceipt {
        let raw = &self.evidence;
        SerpRawReceipt {
            evidence_id: raw.evidence_id,
            measurement_id: raw.measurement_id,
            attempt_id: raw.attempt_id,
            operation: raw.operation,
            provider_task_id: raw.provider_task_id.clone(),
            request_sha256: raw.request_sha256.clone(),
            intent_request_sha256: raw.intent_request_sha256.clone(),
            response_sha256: raw.response_sha256.clone(),
            body_bytes: raw.body.len(),
            body_complete: raw.body_complete,
            http_status: raw.http_status,
            send_certainty: raw.send_certainty,
            captured_at: raw.captured_at,
            stored_at: self.stored_at,
        }
    }
}

impl std::fmt::Debug for SerpRawEvidence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SerpRawEvidence")
            .field("evidence_id", &self.evidence_id)
            .field("operation", &self.operation)
            .field("body_bytes", &self.body.len())
            .field("body_complete", &self.body_complete)
            .finish_non_exhaustive()
    }
}

impl SerpClaim {
    pub fn validate_live(&self, now: DateTime<Utc>) -> Result<(), AppError> {
        if self.claim_token.is_nil()
            || self.attempt_id.is_nil()
            || now < self.claimed_at
            || now >= self.lease_expires_at
        {
            return Err(AppError::conflict("search claim expired or invalid"));
        }
        Ok(())
    }
}

impl SerpSendingIntent {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.measurement_id.is_nil()
            || self.attempt_id.is_nil()
            || self.send_token.is_nil()
            || !digest(&self.request_sha256)
            || !label(&self.correlation_tag, 256)
            || self
                .credential_revision
                .is_some_and(|revision| revision <= 0)
        {
            return Err(invalid());
        }
        Ok(())
    }
}

impl SerpProviderTask {
    /// Binding evidence must additionally be fetched in scope and verified by
    /// the adapter. This structural check alone is not provider proof.
    pub fn validate_binding(&self, intent: &SerpSendingIntent) -> Result<(), AppError> {
        intent.validate()?;
        if self.measurement_id != intent.measurement_id
            || self.attempt_id != intent.attempt_id
            || self.binding_evidence_id.is_nil()
            || !label(&self.provider_task_id, 256)
            || !label(&self.correlation_tag, 256)
            || self.correlation_tag != intent.correlation_tag
        {
            return Err(invalid());
        }
        Ok(())
    }
}

/// The only recovery from Unknown is a read-only, independently verified task
/// binding. Cancellation is not revoked and this never authorizes another POST.
pub fn validate_serp_read_recovery(
    from: SerpTaskState,
    task: &SerpProviderTask,
    intent: &SerpSendingIntent,
) -> Result<(), AppError> {
    task.validate_binding(intent)?;
    if from != SerpTaskState::Unknown {
        return Err(AppError::conflict("search task cannot recover"));
    }
    Ok(())
}

impl SerpRawEvidence {
    pub fn validate(&self, intent: &SerpSendingIntent) -> Result<(), AppError> {
        intent.validate()?;
        if self.evidence_id.is_nil()
            || self.measurement_id != intent.measurement_id
            || self.attempt_id != intent.attempt_id
            || self.intent_request_sha256 != intent.request_sha256
            || (self.operation == SerpEvidenceOperation::Submission
                && self.request_sha256 != intent.request_sha256)
            || match self.operation {
                SerpEvidenceOperation::Submission => self.provider_task_id.is_some(),
                SerpEvidenceOperation::ResultRead | SerpEvidenceOperation::RecoveryRead => !self
                    .provider_task_id
                    .as_ref()
                    .is_some_and(|id| label(id, 256)),
            }
            || !digest(&self.request_sha256)
            || !digest(&self.response_sha256)
            || self.response_sha256 != sha256_hex(&self.body)
            || self.body.len() > MAX_SERP_RAW_BYTES
            || self.captured_at < intent.intended_at
            || self
                .http_status
                .is_some_and(|status| !(100..=599).contains(&status))
        {
            return Err(invalid());
        }
        Ok(())
    }
}

/// Every implementation must enforce the complete operator/tenant/project scope
/// and the documented transactional fences. No method authorizes a blind retry.
#[async_trait]
pub trait SerpRepository: Send + Sync {
    /// Metadata-only report cohort, UUID ASC, limit 1..=100. Enforce full scope,
    /// scheduled [start,end), and created/stored <= evidence_as_of.
    async fn list_report_measurements(
        &self,
        scope: &TenantScope,
        window: &crate::MeasurementPeriodWindow,
        evidence_as_of: DateTime<Utc>,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<crate::MeasurementPeriodSearchIdentity>, AppError>;
    /// Fixed correction cohort, UUID ASC, at most 100 IDs. Missing IDs are omitted.
    async fn get_report_measurements(
        &self,
        scope: &TenantScope,
        measurement_ids: &[Uuid],
    ) -> Result<Vec<crate::MeasurementPeriodSearchIdentity>, AppError>;
    /// Metadata-only joined observations, observation UUID ASC, limit 1..=100,
    /// at most 100 measurement IDs. Both raw and observation repository storage
    /// clocks must be <= evidence_as_of. Never load raw response bodies.
    async fn list_report_observations(
        &self,
        scope: &TenantScope,
        measurement_ids: &[Uuid],
        evidence_as_of: DateTime<Utc>,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<crate::SerpReportObservation>, AppError>;
    /// Current durable identity for restart recovery. Never reconstruct an
    /// attempt/token from client input or mint a new submission authorization.
    async fn get_execution(
        &self,
        scope: &TenantScope,
        measurement_id: Uuid,
    ) -> Result<Option<SerpExecution>, AppError>;
    /// Receipts ordered (stored_at,evidence_id) DESC; limit 1..=100, scoped cursor.
    /// Does not load response bodies, including malformed or unknown receipts.
    async fn list_raw(
        &self,
        scope: &TenantScope,
        measurement_id: Uuid,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<SerpRawReceipt>, AppError>;
    async fn get_observation(
        &self,
        scope: &TenantScope,
        measurement_id: Uuid,
        observation_id: Uuid,
    ) -> Result<Option<SerpObservation>, AppError>;
    /// Persist a pending read's next due time and release its current live fence.
    /// Retain AwaitingResult and the original immutable sending/task identities.
    async fn release_read(
        &self,
        scope: &TenantScope,
        claim: &SerpClaim,
        next_poll_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<(), AppError>;
    /// Back off Unknown read recovery without sending, changing evidence, or
    /// resurrecting cancelled work. Does not move an existing due time earlier.
    async fn defer_recovery(
        &self,
        scope: &TenantScope,
        measurement_id: Uuid,
        next_poll_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<(), AppError>;
    /// Scoped dispatch candidates ordered by measurement ID ASC; limit 1..=100.
    /// Queued scheduled work and due AwaitingResult/Unknown reads only; excludes
    /// active claims. A candidate is never itself a send authorization.
    async fn list_due(
        &self,
        scope: &TenantScope,
        now: DateTime<Utc>,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<SerpMeasurement>, AppError>;
    /// Same key/input replays; changed input conflicts. Store exact query bytes.
    async fn accept(
        &self,
        scope: &TenantScope,
        idempotency_key: &str,
        measurement: SerpMeasurement,
    ) -> Result<SerpMeasurement, AppError>;
    /// Atomic queued-only claim. A sent/unknown attempt is never reclaimed to send.
    async fn claim(
        &self,
        scope: &TenantScope,
        measurement_id: Uuid,
        now: DateTime<Utc>,
        lease_expires_at: DateTime<Utc>,
    ) -> Result<Option<SerpClaim>, AppError>;
    /// Fresh fenced lease for AwaitingResult only, retaining the original
    /// attempt and binding. It authorizes reads/parsing, never another send.
    async fn claim_read(
        &self,
        scope: &TenantScope,
        measurement_id: Uuid,
        now: DateTime<Utc>,
        lease_expires_at: DateTime<Utc>,
    ) -> Result<Option<SerpClaim>, AppError>;
    /// Bounded reconciliation (limit 1..=100): expired Claimed with no intent
    /// returns to Queued; expired Sending/AwaitingResult becomes Unknown.
    /// Invalidate the old fence, retain attempts/evidence, do not send/reclaim.
    async fn expire_claims(
        &self,
        scope: &TenantScope,
        now: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<Uuid>, AppError>;
    /// Renew only the current unexpired fence; old tokens never regain ownership.
    async fn renew_claim(
        &self,
        scope: &TenantScope,
        claim: &SerpClaim,
        now: DateTime<Utc>,
        lease_expires_at: DateTime<Utc>,
    ) -> Result<SerpClaim, AppError>;
    /// Commit a write-once intent before network I/O. Only the fresh commit returns
    /// Some (one-time send authorization). Exact replay returns None, NEVER another
    /// authorization; changed digest/tag or stale claim conflicts.
    async fn begin_send(
        &self,
        scope: &TenantScope,
        claim: &SerpClaim,
        request_sha256: &str,
        correlation_tag: &str,
        credential_revision: Option<i64>,
        now: DateTime<Utc>,
    ) -> Result<Option<SerpSendingIntent>, AppError>;
    /// Audit/recovery metadata only; reading an intent never authorizes sending.
    async fn get_sending_intent(
        &self,
        scope: &TenantScope,
        measurement_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<Option<SerpSendingIntent>, AppError>;
    /// Persist before decoding. Authenticate the fixed send token and exact binding,
    /// not the current claim: a late response can be archived after cancellation,
    /// but cannot revive task state. Same evidence ID/bytes replays, changed bytes conflict.
    async fn append_raw(
        &self,
        scope: &TenantScope,
        intent: &SerpSendingIntent,
        evidence: SerpRawEvidence,
    ) -> Result<SerpStoredRaw, AppError>;
    /// Write-once external task ID from persisted submission evidence verified by
    /// the authenticated adapter/service. Subsequent reads may poll only this ID;
    /// a tag is correlation, never send idempotency. Repository verifies evidence
    /// scope/attempt/digest, not vendor JSON field semantics.
    async fn bind_provider_task(
        &self,
        scope: &TenantScope,
        intent: &SerpSendingIntent,
        task: SerpProviderTask,
    ) -> Result<SerpProviderTask, AppError>;
    /// After authenticated exact-task GET evidence proves tag/protocol/attempt binding,
    /// atomically Unknown -> AwaitingResult. Never re-send; cancelled stays cancelled.
    /// Account-wide discovery bytes MUST NOT be stored in this tenant repository.
    /// Mint no send authorization; caller obtains a new claim_read fence.
    async fn recover_provider_task(
        &self,
        scope: &TenantScope,
        intent: &SerpSendingIntent,
        task: SerpProviderTask,
        now: DateTime<Utc>,
    ) -> Result<SerpProviderTask, AppError>;
    async fn get_provider_task(
        &self,
        scope: &TenantScope,
        measurement_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<Option<SerpProviderTask>, AppError>;
    async fn get_raw(
        &self,
        scope: &TenantScope,
        evidence_id: Uuid,
    ) -> Result<Option<SerpStoredRaw>, AppError>;
    /// Append-only analysis version referencing scoped raw evidence with identical
    /// attempt/digest. Load existing scoped raw evidence before append;
    /// validate_source enforces receipt binding, not repository clock ordering.
    /// Reanalysis never calls begin_send or mutates prior versions.
    async fn append_observation(
        &self,
        scope: &TenantScope,
        observation: SerpObservation,
    ) -> Result<SerpObservation, AppError>;
    /// Fenced transition; raw evidence received late is not a task-state transition.
    async fn finish(
        &self,
        scope: &TenantScope,
        claim: &SerpClaim,
        state: SerpTaskState,
        now: DateTime<Utc>,
    ) -> Result<(), AppError>;
    async fn cancel(
        &self,
        scope: &TenantScope,
        measurement_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<(), AppError>;
    async fn get(
        &self,
        scope: &TenantScope,
        measurement_id: Uuid,
    ) -> Result<Option<SerpMeasurement>, AppError>;
    /// Stable scoped keyset; limit 1..=100, unknown/foreign cursor rejected.
    async fn list(
        &self,
        scope: &TenantScope,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<SerpMeasurement>, AppError>;
    /// Immutable versions ordered (analyzed_at,id) descending, scoped cursor.
    async fn list_observations(
        &self,
        scope: &TenantScope,
        measurement_id: Uuid,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<SerpObservation>, AppError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn source_route_identity_is_required_and_part_of_immutable_input() {
        let scope = TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        );
        let first = measurement();
        first.validate(&scope).unwrap();
        let mut second = first.clone();
        second.source_key = "synthetic-secondary".into();
        second.validate(&scope).unwrap();
        assert_eq!(first.protocol, second.protocol);
        assert!(!first.same_input(&second));
        for invalid_key in ["".into(), " ".into(), "x".repeat(129), "route\nkey".into()] {
            second.source_key = invalid_key;
            assert!(second.validate(&scope).is_err());
        }
    }

    fn measurement() -> SerpMeasurement {
        SerpMeasurement {
            measurement_id: Uuid::new_v4(),
            source_key: "synthetic-primary".into(),
            protocol: SerpProtocol {
                query: "  café + rainfall%  ".into(),
                engine: SerpEngine::Google,
                surface: SerpSurface::ThirdPartyApi,
                source: "synthetic".into(),
                source_location_code: "2840".into(),
                country: "US".into(),
                city: None,
                language: "en".into(),
                device: SerpDevice::Desktop,
                operating_system: "windows".into(),
                requested_depth: 10,
                max_pages: 1,
                priority: 1,
                login: "unspecified".into(),
                personalization: "unspecified".into(),
                protocol_version: SERP_PROTOCOL_VERSION.into(),
                connector_version: "synthetic.v1".into(),
            },
            target: None,
            target_rule_version: SERP_TARGET_RULE_VERSION.into(),
            question_binding: None,
            scheduled_at: Utc::now(),
            created_at: Utc::now(),
            state: SerpTaskState::Queued,
        }
    }

    fn organic(rank: u32, url: &str) -> SerpResult {
        let (normalized_url, host) = normalize_serp_url(url).unwrap();
        SerpResult {
            kind: SerpResultKind::Organic,
            raw_kind: "organic".into(),
            raw_url: Some(url.into()),
            normalized_url: Some(normalized_url),
            host: Some(host),
            normalization_version: SERP_URL_RULE_VERSION.into(),
            title: Some("Synthetic result".into()),
            page: Some(1),
            position: rank,
            organic_rank: Some(rank),
            absolute_position: Some(rank + 2),
            locator: format!("/tasks/0/result/0/items/{}", rank - 1),
        }
    }

    fn observation(measurement: &SerpMeasurement, count: u32) -> SerpObservation {
        let now = Utc::now();
        SerpObservation {
            observation_id: Uuid::new_v4(),
            measurement_id: measurement.measurement_id,
            attempt_id: Uuid::new_v4(),
            raw_evidence_id: Uuid::new_v4(),
            raw_sha256: "a".repeat(64),
            parser_version: "synthetic.v1".into(),
            provider_observed_at: None,
            received_at: now,
            analyzed_at: now,
            actual_conditions: SerpActualConditions::default(),
            status: if count == 10 {
                SerpObservationStatus::Observed
            } else {
                SerpObservationStatus::Partial
            },
            coverage: SerpCoverage {
                requested_depth: 10,
                observed_organic_depth: count,
                pages_received: 1,
                completion: if count == 10 {
                    SerpCoverageCompletion::RequestedDepth
                } else {
                    SerpCoverageCompletion::Partial
                },
                truncated: count < 10,
                exhaustion_evidence_locator: None,
            },
            results: (1..=count)
                .map(|rank| organic(rank, &format!("https://example.org/{rank}")))
                .collect(),
            source_limitations: vec![SerpLimitationCode::RequestedConditionsUnverified],
        }
    }

    #[test]
    fn optional_target_and_exact_query_are_valid_without_setup() {
        let measurement = measurement();
        let scope = TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        );
        measurement.validate(&scope).unwrap();
        assert_eq!(
            observation(&measurement, 10)
                .target_match(&measurement)
                .unwrap(),
            SerpTargetMatch::NotRequested
        );
        let mut trimmed = measurement.clone();
        trimmed.protocol.query = trimmed.protocol.query.trim().into();
        assert!(!measurement.same_input(&trimmed));
        assert_eq!(measurement.protocol.query, "  café + rainfall%  ");
        let mut projectless = scope;
        projectless.project_id = None;
        assert!(measurement.validate(&projectless).is_err());
    }

    #[test]
    fn partial_hit_and_complete_absence_never_invent_zero_rank() {
        let mut measurement = measurement();
        measurement.target = Some(SerpTarget::Url {
            url: "https://example.org/2".into(),
        });
        let partial = observation(&measurement, 3);
        assert_eq!(
            partial.target_match(&measurement).unwrap(),
            SerpTargetMatch::Hit {
                organic_ranks: vec![2]
            }
        );
        measurement.target = Some(SerpTarget::Url {
            url: "https://example.org/absent".into(),
        });
        assert_eq!(
            partial.target_match(&measurement).unwrap(),
            SerpTargetMatch::Undetermined
        );
        assert_eq!(
            observation(&measurement, 10)
                .target_match(&measurement)
                .unwrap(),
            SerpTargetMatch::NotFoundWithinDepth { covered_depth: 10 }
        );
        let mut falsely_complete = partial;
        falsely_complete.status = SerpObservationStatus::Observed;
        falsely_complete.coverage.completion = SerpCoverageCompletion::RequestedDepth;
        falsely_complete.coverage.truncated = false;
        assert!(falsely_complete.validate(&measurement).is_err());
    }

    #[test]
    fn special_results_do_not_occupy_organic_rank_and_duplicates_remain() {
        let measurement = measurement();
        let mut value = observation(&measurement, 2);
        value.results[1] = organic(2, "https://example.org/1");
        let mut ad = organic(3, "https://ads.example.org/");
        ad.kind = SerpResultKind::Advertisement;
        ad.raw_kind = "paid".into();
        assert!(ad.validate().is_err());
        ad.organic_rank = None;
        value.results.push(ad);
        let special = SerpResult {
            kind: SerpResultKind::AiOverview,
            raw_kind: "ai_overview".into(),
            raw_url: None,
            normalized_url: None,
            host: None,
            normalization_version: SERP_URL_RULE_VERSION.into(),
            title: None,
            page: None,
            position: 4,
            organic_rank: None,
            absolute_position: None,
            locator: "/tasks/0/result/0/items/3".into(),
        };
        value.results.push(special);
        value.validate(&measurement).unwrap();
        assert_eq!(value.results.len(), 4);
        assert_eq!(value.coverage.observed_organic_depth, 2);
        assert_eq!(
            value.results[0].normalized_url,
            value.results[1].normalized_url
        );
    }

    #[test]
    fn url_matching_is_explicit_boundary_safe_and_versioned() {
        let result = organic(1, "https://News.Example.org:443/a?b=2&a=1#section");
        assert_eq!(
            result.normalized_url.as_deref(),
            Some("https://news.example.org/a?b=2&a=1")
        );
        let host = SerpTarget::Host {
            host: "EXAMPLE.org".into(),
            include_subdomains: true,
        };
        assert!(host.matches(&result).unwrap());
        assert!(
            !host
                .matches(&organic(1, "https://notexample.org/a"))
                .unwrap()
        );
        assert!(
            !host
                .matches(&organic(1, "https://example.org.evil.test/a"))
                .unwrap()
        );
        let exact = SerpTarget::Url {
            url: "https://news.example.org/a?b=2&a=1#other".into(),
        };
        assert!(exact.matches(&result).unwrap());
        assert!(
            !exact
                .matches(&organic(1, "https://news.example.org/a?a=1&b=2"))
                .unwrap()
        );
        assert!(normalize_serp_url("https://secret@example.org/").is_err());
        assert!(normalize_serp_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn missing_urls_and_failed_sampling_cannot_become_negative_findings() {
        let mut measurement = measurement();
        measurement.target = Some(SerpTarget::Host {
            host: "absent.example.org".into(),
            include_subdomains: false,
        });
        let mut value = observation(&measurement, 10);
        value.results[9].raw_url = None;
        value.results[9].normalized_url = None;
        value.results[9].host = None;
        assert!(value.validate(&measurement).is_err());
        for status in [
            SerpObservationStatus::Missing,
            SerpObservationStatus::Failed,
            SerpObservationStatus::Challenge,
            SerpObservationStatus::LoginRequired,
            SerpObservationStatus::Unsupported,
        ] {
            let mut failed = observation(&measurement, 0);
            failed.status = status;
            assert_eq!(
                failed.target_match(&measurement).unwrap(),
                SerpTargetMatch::Undetermined
            );
        }
    }

    #[test]
    fn protocol_trends_preserve_unknown_actual_conditions() {
        let measurement = measurement();
        let value = observation(&measurement, 10);
        assert!(serp_samples_comparable(
            &measurement,
            &value,
            &measurement,
            &value
        ));
        assert!(
            !compare_serp_samples(&measurement, &value, &measurement, &value)
                .actual_conditions_verified
        );
        let mut verified = value.clone();
        let condition = Some(SerpActualCondition {
            value: "synthetic_verified".into(),
            evidence_locator: "/actual".into(),
        });
        verified.actual_conditions = SerpActualConditions {
            country: condition.clone(),
            city: None,
            language: condition.clone(),
            device: condition.clone(),
            login: condition.clone(),
            personalization: condition,
        };
        assert!(serp_samples_comparable(
            &measurement,
            &verified,
            &measurement,
            &verified
        ));
        assert!(
            compare_serp_samples(&measurement, &verified, &measurement, &verified)
                .actual_conditions_verified
        );
        let mut changed = measurement.clone();
        changed.protocol.connector_version = "synthetic.v2".into();
        assert!(!serp_samples_comparable(
            &measurement,
            &verified,
            &changed,
            &verified
        ));
    }

    #[test]
    fn write_once_sending_and_late_evidence_binding_are_separate() {
        let now = Utc::now();
        let intent = SerpSendingIntent {
            measurement_id: Uuid::new_v4(),
            attempt_id: Uuid::new_v4(),
            send_token: Uuid::new_v4(),
            request_sha256: "a".repeat(64),
            correlation_tag: "opaque-synthetic-tag".into(),
            credential_revision: Some(1),
            intended_at: now,
        };
        let mut raw = SerpRawEvidence {
            evidence_id: Uuid::new_v4(),
            measurement_id: intent.measurement_id,
            attempt_id: intent.attempt_id,
            operation: SerpEvidenceOperation::Submission,
            provider_task_id: None,
            request_sha256: intent.request_sha256.clone(),
            intent_request_sha256: intent.request_sha256.clone(),
            response_sha256: sha256_hex(b"{}"),
            body: b"{}".to_vec(),
            body_complete: true,
            http_status: Some(200),
            send_certainty: SerpSendCertainty::ResponseReceived,
            captured_at: now + Duration::minutes(10),
        };
        raw.validate(&intent).unwrap();
        assert!(!format!("{raw:?}").contains("{}"));
        let task = SerpProviderTask {
            measurement_id: intent.measurement_id,
            attempt_id: intent.attempt_id,
            binding_evidence_id: raw.evidence_id,
            provider_task_id: "synthetic-task".into(),
            correlation_tag: intent.correlation_tag.clone(),
        };
        validate_serp_read_recovery(SerpTaskState::Unknown, &task, &intent).unwrap();
        assert!(validate_serp_read_recovery(SerpTaskState::Cancelled, &task, &intent).is_err());
        for terminal in [
            SerpTaskState::Unknown,
            SerpTaskState::Cancelled,
            SerpTaskState::Completed,
            SerpTaskState::Failed,
        ] {
            assert!(validate_serp_task_transition(terminal, SerpTaskState::Sending).is_err());
        }
        validate_serp_task_transition(SerpTaskState::Unknown, SerpTaskState::Cancelled).unwrap();
        raw.attempt_id = Uuid::new_v4();
        assert!(raw.validate(&intent).is_err());
        raw.attempt_id = intent.attempt_id;
        raw.body.push(b' ');
        assert!(raw.validate(&intent).is_err());
    }

    #[test]
    fn lease_deadline_and_raw_binding_are_authoritative_without_cross_host_clock_order() {
        let measurement = measurement();
        let mut value = observation(&measurement, 10);
        let claim = SerpClaim {
            measurement: measurement.clone(),
            attempt_id: value.attempt_id,
            claim_token: Uuid::new_v4(),
            claimed_at: value.received_at - Duration::seconds(1),
            lease_expires_at: value.received_at + Duration::seconds(1),
        };
        claim.validate_live(value.received_at).unwrap();
        assert!(claim.validate_live(claim.lease_expires_at).is_err());
        let mut raw = SerpStoredRaw {
            evidence: SerpRawEvidence {
                evidence_id: value.raw_evidence_id,
                measurement_id: measurement.measurement_id,
                attempt_id: value.attempt_id,
                operation: SerpEvidenceOperation::ResultRead,
                provider_task_id: Some("synthetic-task".into()),
                request_sha256: "b".repeat(64),
                intent_request_sha256: "c".repeat(64),
                response_sha256: value.raw_sha256.clone(),
                body: vec![],
                body_complete: true,
                http_status: Some(200),
                send_certainty: SerpSendCertainty::ResponseReceived,
                captured_at: value.received_at,
            },
            stored_at: value.received_at + Duration::seconds(1),
        };
        let original = value.clone();
        for skew in [-20, 20] {
            raw.stored_at = value.received_at + Duration::seconds(skew);
            value.validate_source(&measurement, &raw).unwrap();
            assert_eq!(value, original);
        }
        for field in 0..4 {
            let mut mismatched = raw.clone();
            match field {
                0 => mismatched.evidence.evidence_id = Uuid::new_v4(),
                1 => mismatched.evidence.measurement_id = Uuid::new_v4(),
                2 => mismatched.evidence.attempt_id = Uuid::new_v4(),
                3 => mismatched.evidence.response_sha256 = "b".repeat(64),
                _ => unreachable!(),
            }
            assert!(value.validate_source(&measurement, &mismatched).is_err());
        }
        value.analyzed_at = value.received_at - Duration::seconds(1);
        assert!(value.validate_source(&measurement, &raw).is_err());
        value = original;
        raw.evidence.body_complete = false;
        assert!(value.validate_source(&measurement, &raw).is_err());
        raw.evidence.body_complete = true;
        raw.evidence.operation = SerpEvidenceOperation::Submission;
        raw.evidence.provider_task_id = None;
        assert!(value.validate_source(&measurement, &raw).is_err());
    }
}
