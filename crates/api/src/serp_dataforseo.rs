//! Exact-byte DataForSEO mapping. Lifecycle, authorization and persistence stay
//! in the shared SERP service; this adapter decodes only stored raw receipts.
use std::{collections::BTreeSet, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, SERP_PROTOCOL_VERSION, SERP_URL_RULE_VERSION, SerpActualConditions, SerpCoverage,
    SerpCoverageCompletion, SerpDevice, SerpEngine, SerpEvidenceOperation, SerpLimitationCode,
    SerpMeasurement, SerpObservation, SerpObservationStatus, SerpProtocol, SerpResult,
    SerpResultKind, SerpSendCertainty, SerpSendingIntent, SerpStoredRaw, SerpSurface,
    normalize_serp_url,
};
use geo_provider::{
    dataforseo::{
        DATAFORSEO_SERP_CONNECTOR_VERSION, DATAFORSEO_SERP_PARSER_VERSION, DataForSeoClient,
        DataForSeoPreparedTask, DataForSeoTaskOutcome, DataForSeoTaskRequest,
        dataforseo_read_request_sha256, decode_dataforseo_get, decode_dataforseo_post,
    },
    serp::{SerpItemKind, SerpOperation, SerpRawResponse, SerpSentCertainty, SerpTransportError},
};
use serde_json::Value;
use uuid::Uuid;

use crate::serp::{SerpPreparedSubmission, SerpReadOutcome, SerpSource};

/// Trusted deployment/source-catalog configuration, never model-selected.
#[derive(Clone)]
pub struct DataForSeoSerpConfig {
    pub location_code: u32,
    pub country: String,
    pub city: Option<String>,
    pub language_code: String,
}

pub struct DataForSeoSerpSource {
    client: Arc<DataForSeoClient>,
    config: DataForSeoSerpConfig,
}

fn invalid() -> AppError {
    AppError::invalid_request("search provider evidence binding invalid")
}

impl DataForSeoSerpSource {
    pub fn new(
        client: Arc<DataForSeoClient>,
        config: DataForSeoSerpConfig,
    ) -> Result<Self, AppError> {
        let source = Self { client, config };
        source.protocol("synthetic validation").validate()?;
        source
            .request("synthetic validation", "synthetic-validation")
            .prepare()
            .map_err(|_| invalid())?;
        Ok(source)
    }

    fn request(&self, query: &str, tag: &str) -> DataForSeoTaskRequest {
        DataForSeoTaskRequest {
            keyword: query.into(),
            location_code: self.config.location_code,
            language_code: self.config.language_code.clone(),
            tag: tag.into(),
        }
    }

    fn validate_measurement(&self, measurement: &SerpMeasurement) -> Result<(), AppError> {
        measurement.protocol.validate()?;
        if measurement.protocol != self.protocol(&measurement.protocol.query) {
            return Err(invalid());
        }
        Ok(())
    }

    fn prepared_for(
        &self,
        measurement: &SerpMeasurement,
        tag: &str,
    ) -> Result<DataForSeoPreparedTask, AppError> {
        self.validate_measurement(measurement)?;
        self.request(&measurement.protocol.query, tag)
            .prepare()
            .map_err(|_| invalid())
    }

    fn validate_raw(
        &self,
        measurement: &SerpMeasurement,
        raw: &SerpStoredRaw,
        intent: &SerpSendingIntent,
    ) -> Result<(), AppError> {
        raw.evidence.validate(intent)?;
        if measurement.measurement_id != intent.measurement_id
            || raw.stored_at < raw.evidence.captured_at
            || self
                .prepared_for(measurement, &intent.correlation_tag)?
                .request_sha256()
                != intent.request_sha256
        {
            return Err(invalid());
        }
        Ok(())
    }

    fn verify_echo(
        &self,
        measurement: &SerpMeasurement,
        raw: &SerpStoredRaw,
        intent: &SerpSendingIntent,
    ) -> Result<(), AppError> {
        let response: Value = serde_json::from_slice(&raw.evidence.body).map_err(|_| invalid())?;
        let prepared = self.prepared_for(measurement, &intent.correlation_tag)?;
        let request: Value = serde_json::from_slice(prepared.body()).map_err(|_| invalid())?;
        let echo = &response["tasks"][0]["data"];
        let expected = request[0].as_object().ok_or_else(invalid)?;
        if expected
            .iter()
            .any(|(key, value)| echo.get(key) != Some(value))
            || echo.get("se").is_some_and(|value| value != "google")
            || echo.get("se_type").is_some_and(|value| value != "organic")
        {
            return Err(invalid());
        }
        Ok(())
    }
}

fn provider_raw(raw: &SerpStoredRaw, operation: SerpOperation) -> SerpRawResponse {
    SerpRawResponse {
        operation,
        http_status: raw.evidence.http_status,
        body: raw.evidence.body.clone(),
        body_complete: raw.evidence.body_complete,
        sent: match raw.evidence.send_certainty {
            SerpSendCertainty::NotSent => SerpSentCertainty::NotSent,
            SerpSendCertainty::MayHaveBeenSent => SerpSentCertainty::PossiblySent,
            SerpSendCertainty::ResponseReceived => SerpSentCertainty::ResponseReceived,
        },
        error: None,
    }
}

#[async_trait]
impl SerpSource for DataForSeoSerpSource {
    fn protocol(&self, query: &str) -> SerpProtocol {
        SerpProtocol {
            query: query.into(),
            engine: SerpEngine::Google,
            surface: SerpSurface::ThirdPartyApi,
            source: "dataforseo".into(),
            source_location_code: self.config.location_code.to_string(),
            country: self.config.country.clone(),
            city: self.config.city.clone(),
            language: self.config.language_code.clone(),
            device: SerpDevice::Desktop,
            operating_system: "windows".into(),
            requested_depth: 10,
            max_pages: 1,
            priority: 1,
            login: "unspecified".into(),
            personalization: "unspecified".into(),
            protocol_version: SERP_PROTOCOL_VERSION.into(),
            connector_version: DATAFORSEO_SERP_CONNECTOR_VERSION.into(),
        }
    }

    fn parser_version(&self) -> &str {
        DATAFORSEO_SERP_PARSER_VERSION
    }

    fn prepare(
        &self,
        measurement: &SerpMeasurement,
        correlation_tag: &str,
    ) -> Result<SerpPreparedSubmission, AppError> {
        let prepared = self.prepared_for(measurement, correlation_tag)?;
        Ok(SerpPreparedSubmission {
            request_sha256: prepared.request_sha256().into(),
            correlation_tag: prepared.tag().into(),
            body: prepared.body().to_vec(),
        })
    }

    async fn send(&self, prepared: &SerpPreparedSubmission) -> SerpRawResponse {
        if let Ok(wire) = DataForSeoPreparedTask::from_bytes(prepared.body.clone())
            && wire.request_sha256() == prepared.request_sha256
            && wire.tag() == prepared.correlation_tag
        {
            return self.client.post_prepared(&wire).await;
        }
        SerpRawResponse {
            operation: SerpOperation::PostTask,
            http_status: None,
            body: vec![],
            body_complete: false,
            sent: SerpSentCertainty::NotSent,
            error: Some(SerpTransportError::InvalidRequest),
        }
    }

    fn read_request_sha256(&self, task_id: &str) -> Result<String, AppError> {
        dataforseo_read_request_sha256(task_id).map_err(|_| invalid())
    }

    async fn read(&self, task_id: &str) -> SerpRawResponse {
        self.client.get_task(task_id).await
    }

    fn decode_submission(
        &self,
        measurement: &SerpMeasurement,
        raw: &SerpStoredRaw,
        intent: &SerpSendingIntent,
    ) -> Result<String, AppError> {
        self.validate_raw(measurement, raw, intent)?;
        if raw.evidence.operation != SerpEvidenceOperation::Submission {
            return Err(invalid());
        }
        let posted = decode_dataforseo_post(
            &provider_raw(raw, SerpOperation::PostTask),
            &intent.correlation_tag,
        )
        .map_err(|_| invalid())?;
        self.verify_echo(measurement, raw, intent)?;
        Ok(posted.task_id)
    }

    fn decode_result(
        &self,
        measurement: &SerpMeasurement,
        raw: &SerpStoredRaw,
        intent: &SerpSendingIntent,
        task_id: &str,
        observation_id: Uuid,
        analyzed_at: DateTime<Utc>,
    ) -> Result<SerpReadOutcome, AppError> {
        self.validate_raw(measurement, raw, intent)?;
        if !matches!(
            raw.evidence.operation,
            SerpEvidenceOperation::ResultRead | SerpEvidenceOperation::RecoveryRead
        ) || raw.evidence.provider_task_id.as_deref() != Some(task_id)
            || raw.evidence.request_sha256 != self.read_request_sha256(task_id)?
        {
            return Err(invalid());
        }
        let decoded = decode_dataforseo_get(&provider_raw(raw, SerpOperation::GetTask), task_id)
            .map_err(|_| invalid())?;
        let provider = match decoded {
            DataForSeoTaskOutcome::Pending { .. } => return Ok(SerpReadOutcome::Pending),
            DataForSeoTaskOutcome::Failed { .. } => return Ok(SerpReadOutcome::Failed),
            DataForSeoTaskOutcome::Completed { result } => result,
        };
        self.verify_echo(measurement, raw, intent)?;
        let mut limitations = vec![
            SerpLimitationCode::RequestedConditionsUnverified,
            SerpLimitationCode::LoginUnknown,
            SerpLimitationCode::PersonalizationUnknown,
            SerpLimitationCode::SinglePageLimit,
        ];
        let mut results = Vec::with_capacity(provider.items.len());
        for (index, item) in provider.items.into_iter().enumerate() {
            let normalized = item
                .raw_url
                .as_deref()
                .and_then(|raw| normalize_serp_url(raw).ok());
            if item.raw_url.is_some()
                && normalized.is_none()
                && !limitations.contains(&SerpLimitationCode::InvalidResultUrl)
            {
                limitations.push(SerpLimitationCode::InvalidResultUrl);
            }
            results.push(SerpResult {
                kind: match item.kind {
                    SerpItemKind::Organic => SerpResultKind::Organic,
                    SerpItemKind::Paid => SerpResultKind::Advertisement,
                    SerpItemKind::FeaturedSnippet => SerpResultKind::FeaturedSnippet,
                    SerpItemKind::LocalPack => SerpResultKind::Maps,
                    SerpItemKind::AiOverview => SerpResultKind::AiOverview,
                    SerpItemKind::Other => SerpResultKind::Other,
                },
                raw_kind: item.raw_kind,
                raw_url: item.raw_url,
                normalized_url: normalized.as_ref().map(|value| value.0.clone()),
                host: normalized.map(|value| value.1),
                normalization_version: SERP_URL_RULE_VERSION.into(),
                title: item.title,
                page: item.page,
                position: index as u32 + 1,
                organic_rank: item.organic_rank,
                absolute_position: item.absolute_position,
                locator: item.evidence_pointer,
            });
        }
        let ranks: BTreeSet<u32> = results
            .iter()
            .filter(|result| result.normalized_url.is_some())
            .filter_map(|result| result.organic_rank)
            .collect();
        let contiguous = (1..).take_while(|rank| ranks.contains(rank)).count() as u32;
        let full = contiguous == measurement.protocol.requested_depth
            && results
                .iter()
                .filter(|result| result.kind == SerpResultKind::Organic)
                .count()
                == contiguous as usize
            && raw.evidence.body_complete;
        if !full {
            limitations.push(SerpLimitationCode::MissingPositions);
        }
        let pages_received = provider
            .provider_reported_pages
            .or_else(|| results.iter().filter_map(|result| result.page).max())
            .unwrap_or(0);
        let provider_observed_at = provider
            .provider_reported_datetime
            .as_deref()
            .and_then(|value| {
                DateTime::parse_from_rfc3339(value)
                    .or_else(|_| DateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S %:z"))
                    .ok()
            })
            .map(|value| value.with_timezone(&Utc));
        if provider_observed_at.is_some() {
            limitations.push(SerpLimitationCode::ProviderReportedTime);
        }
        let observation = SerpObservation {
            observation_id,
            measurement_id: measurement.measurement_id,
            attempt_id: intent.attempt_id,
            raw_evidence_id: raw.evidence.evidence_id,
            raw_sha256: raw.evidence.response_sha256.clone(),
            parser_version: self.parser_version().into(),
            provider_observed_at,
            received_at: raw.evidence.captured_at,
            analyzed_at,
            status: if full {
                SerpObservationStatus::Observed
            } else {
                SerpObservationStatus::Partial
            },
            actual_conditions: SerpActualConditions::default(),
            coverage: SerpCoverage {
                requested_depth: measurement.protocol.requested_depth,
                observed_organic_depth: contiguous,
                pages_received,
                completion: if full {
                    SerpCoverageCompletion::RequestedDepth
                } else {
                    SerpCoverageCompletion::Partial
                },
                truncated: !full,
                exhaustion_evidence_locator: None,
            },
            results,
            source_limitations: limitations,
        };
        observation.validate_source(measurement, raw)?;
        Ok(SerpReadOutcome::Observation(Box::new(observation)))
    }

    fn verify_recovery(
        &self,
        measurement: &SerpMeasurement,
        raw: &SerpStoredRaw,
        intent: &SerpSendingIntent,
        task_id: &str,
    ) -> Result<(), AppError> {
        self.validate_raw(measurement, raw, intent)?;
        if raw.evidence.operation != SerpEvidenceOperation::RecoveryRead
            || raw.evidence.provider_task_id.as_deref() != Some(task_id)
            || raw.evidence.request_sha256 != self.read_request_sha256(task_id)?
        {
            return Err(invalid());
        }
        decode_dataforseo_get(&provider_raw(raw, SerpOperation::GetTask), task_id)
            .map_err(|_| invalid())?;
        self.verify_echo(measurement, raw, intent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo_domain::{
        SERP_TARGET_RULE_VERSION, SerpRawEvidence, SerpTarget, SerpTargetMatch, SerpTaskState,
        sha256_hex,
    };
    use serde_json::json;

    const TASK_ID: &str = "00000000-0000-0000-0000-000000000001";

    fn source() -> DataForSeoSerpSource {
        DataForSeoSerpSource::new(
            Arc::new(
                DataForSeoClient::new("synthetic-login".into(), "synthetic-password".into())
                    .unwrap(),
            ),
            DataForSeoSerpConfig {
                location_code: 2840,
                country: "US".into(),
                city: None,
                language_code: "en".into(),
            },
        )
        .unwrap()
    }

    fn fixture(source: &DataForSeoSerpSource) -> (SerpMeasurement, SerpSendingIntent) {
        let now = Utc::now() - chrono::Duration::seconds(5);
        let measurement = SerpMeasurement {
            measurement_id: Uuid::new_v4(),
            source_key: "synthetic-source".into(),
            protocol: source.protocol("  rain + gauge 100% "),
            target: Some(SerpTarget::Host {
                host: "absent.example.org".into(),
                include_subdomains: false,
            }),
            target_rule_version: SERP_TARGET_RULE_VERSION.into(),
            question_binding: None,
            scheduled_at: now,
            created_at: now,
            state: SerpTaskState::Queued,
        };
        let prepared = source
            .prepare(&measurement, "synthetic-correlation")
            .unwrap();
        let intent = SerpSendingIntent {
            measurement_id: measurement.measurement_id,
            attempt_id: Uuid::new_v4(),
            send_token: Uuid::new_v4(),
            request_sha256: prepared.request_sha256,
            correlation_tag: prepared.correlation_tag,
            intended_at: now,
        };
        (measurement, intent)
    }

    fn stored(
        source: &DataForSeoSerpSource,
        measurement: &SerpMeasurement,
        intent: &SerpSendingIntent,
        operation: SerpEvidenceOperation,
        status: u32,
        items: Value,
    ) -> SerpStoredRaw {
        let prepared = source
            .prepare(measurement, &intent.correlation_tag)
            .unwrap();
        let request: Value = serde_json::from_slice(&prepared.body).unwrap();
        let body = serde_json::to_vec(&json!({
            "status_code":20000,"tasks_count":1,"tasks_error":0,
            "tasks":[{"id":TASK_ID,"status_code":status,"data":request[0],
                "result":[{"datetime":"2026-01-01 00:00:00 +00:00","pages_count":1,
                    "items_count":items.as_array().map_or(0,Vec::len),"items":items}]}],
        }))
        .unwrap();
        let captured_at = intent.intended_at + chrono::Duration::seconds(1);
        SerpStoredRaw {
            evidence: SerpRawEvidence {
                evidence_id: Uuid::new_v4(),
                measurement_id: measurement.measurement_id,
                attempt_id: intent.attempt_id,
                operation,
                provider_task_id: (operation != SerpEvidenceOperation::Submission)
                    .then(|| TASK_ID.into()),
                request_sha256: if operation == SerpEvidenceOperation::Submission {
                    intent.request_sha256.clone()
                } else {
                    source.read_request_sha256(TASK_ID).unwrap()
                },
                intent_request_sha256: intent.request_sha256.clone(),
                response_sha256: sha256_hex(&body),
                body,
                body_complete: true,
                http_status: Some(200),
                send_certainty: SerpSendCertainty::ResponseReceived,
                captured_at,
            },
            stored_at: captured_at,
        }
    }

    fn items(ranks: &[u32]) -> Value {
        Value::Array(
            ranks
                .iter()
                .map(|rank| {
                    json!({
                        "type":"organic","rank_group":rank,"rank_absolute":rank+2,"page":1,
                        "url":"https://example.org/same#fragment","title":"Synthetic result",
                    })
                })
                .collect(),
        )
    }

    #[test]
    fn full_rank_coverage_keeps_echoes_unknown_and_points_into_exact_saved_bytes() {
        let source = source();
        let (measurement, intent) = fixture(&source);
        let mut list = items(&(1..=10).collect::<Vec<_>>());
        list.as_array_mut().unwrap().insert(0, json!({"type":"paid","rank_group":1,"rank_absolute":1,"page":1,"url":"https://example.org/ad"}));
        list.as_array_mut()
            .unwrap()
            .push(json!({"type":"new_kind","rank_absolute":13,"page":1}));
        let raw = stored(
            &source,
            &measurement,
            &intent,
            SerpEvidenceOperation::ResultRead,
            20000,
            list,
        );
        let snapshot = raw.clone();
        let SerpReadOutcome::Observation(observation) = source
            .decode_result(
                &measurement,
                &raw,
                &intent,
                TASK_ID,
                Uuid::new_v4(),
                Utc::now(),
            )
            .unwrap()
        else {
            panic!("observation")
        };
        assert_eq!(observation.status, SerpObservationStatus::Observed);
        assert_eq!(observation.coverage.observed_organic_depth, 10);
        assert_eq!(
            observation.actual_conditions,
            SerpActualConditions::default()
        );
        assert_eq!(observation.results[0].kind, SerpResultKind::Advertisement);
        assert!(observation.results[0].organic_rank.is_none());
        assert_eq!(observation.results[1].organic_rank, Some(1));
        assert_eq!(observation.results[1].absolute_position, Some(3));
        assert_eq!(
            observation.results[1].raw_url,
            observation.results[2].raw_url
        );
        assert_eq!(observation.results[11].raw_kind, "new_kind");
        assert_eq!(observation.raw_sha256, sha256_hex(&raw.evidence.body));
        let json: Value = serde_json::from_slice(&raw.evidence.body).unwrap();
        assert_eq!(
            json.pointer(&observation.results[1].locator).unwrap()["rank_group"],
            1
        );
        assert_eq!(
            observation.target_match(&measurement).unwrap(),
            SerpTargetMatch::NotFoundWithinDepth { covered_depth: 10 }
        );
        assert_eq!(raw, snapshot);
    }

    #[test]
    fn gap_invalid_url_and_provider_no_results_never_become_complete_absence() {
        let source = source();
        let (measurement, intent) = fixture(&source);
        for mut list in [items(&[1, 3]), items(&[1, 2])] {
            if list[1]["rank_group"] == 2 {
                list[1]["url"] = json!("javascript:invalid");
            }
            let raw = stored(
                &source,
                &measurement,
                &intent,
                SerpEvidenceOperation::ResultRead,
                20000,
                list,
            );
            let SerpReadOutcome::Observation(observation) = source
                .decode_result(
                    &measurement,
                    &raw,
                    &intent,
                    TASK_ID,
                    Uuid::new_v4(),
                    Utc::now(),
                )
                .unwrap()
            else {
                panic!("observation")
            };
            assert_eq!(observation.status, SerpObservationStatus::Partial);
            assert_eq!(observation.coverage.observed_organic_depth, 1);
            assert_eq!(
                observation.target_match(&measurement).unwrap(),
                SerpTargetMatch::Undetermined
            );
        }
        let raw = stored(
            &source,
            &measurement,
            &intent,
            SerpEvidenceOperation::ResultRead,
            40102,
            json!([]),
        );
        assert!(matches!(
            source
                .decode_result(
                    &measurement,
                    &raw,
                    &intent,
                    TASK_ID,
                    Uuid::new_v4(),
                    Utc::now()
                )
                .unwrap(),
            SerpReadOutcome::Failed
        ));
    }

    #[test]
    fn submission_and_recovery_require_frozen_protocol_tag_task_and_raw_bindings() {
        let source = source();
        let (measurement, intent) = fixture(&source);
        let post = stored(
            &source,
            &measurement,
            &intent,
            SerpEvidenceOperation::Submission,
            20100,
            json!([]),
        );
        assert_eq!(
            source
                .decode_submission(&measurement, &post, &intent)
                .unwrap(),
            TASK_ID
        );
        let raw = stored(
            &source,
            &measurement,
            &intent,
            SerpEvidenceOperation::RecoveryRead,
            20000,
            items(&[1]),
        );
        source
            .verify_recovery(&measurement, &raw, &intent, TASK_ID)
            .unwrap();
        for field in [
            "tag",
            "keyword",
            "location_code",
            "language_code",
            "device",
            "priority",
        ] {
            let mut changed = raw.clone();
            let mut body: Value = serde_json::from_slice(&changed.evidence.body).unwrap();
            body["tasks"][0]["data"][field] = json!("mismatch");
            changed.evidence.body = serde_json::to_vec(&body).unwrap();
            changed.evidence.response_sha256 = sha256_hex(&changed.evidence.body);
            assert!(
                source
                    .verify_recovery(&measurement, &changed, &intent, TASK_ID)
                    .is_err()
            );
        }
        let mut corrupt = raw.clone();
        corrupt.evidence.body.push(b' ');
        assert!(
            source
                .verify_recovery(&measurement, &corrupt, &intent, TASK_ID)
                .is_err()
        );
        let mut partial = raw.clone();
        partial.evidence.body_complete = false;
        assert!(
            source
                .verify_recovery(&measurement, &partial, &intent, TASK_ID)
                .is_err()
        );
        assert!(
            source
                .verify_recovery(&measurement, &raw, &intent, "different-task")
                .is_err()
        );
        let mut changed = measurement.clone();
        changed.protocol.country = "CA".into();
        assert!(
            source
                .verify_recovery(&changed, &raw, &intent, TASK_ID)
                .is_err()
        );
        assert!(
            source
                .decode_submission(&measurement, &raw, &intent)
                .is_err()
        );
    }

    #[tokio::test]
    async fn tampered_prepared_bytes_or_hash_do_not_send() {
        let source = source();
        let (measurement, _) = fixture(&source);
        let mut prepared = source
            .prepare(&measurement, "synthetic-correlation")
            .unwrap();
        assert_eq!(prepared.request_sha256, sha256_hex(&prepared.body));
        prepared.body.push(b' ');
        let response = source.send(&prepared).await;
        assert_eq!(response.sent, SerpSentCertainty::NotSent);
        assert_eq!(response.error, Some(SerpTransportError::InvalidRequest));
        assert!(source.read_request_sha256("../another").is_err());
    }
}
