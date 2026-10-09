//! Synthetic adapters only: no search supplier or model calls.
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_api::{
    AppState, DataForSeoSerpConfig, DataForSeoSerpSource, RepositoryHostOps,
    SerpPreparedSubmission, SerpReadOutcome, SerpService, SerpSource,
};
use geo_domain::*;
use geo_persistence::MemorySerpRepository;
use geo_provider::{
    dataforseo::DataForSeoClient,
    serp::{SerpOperation, SerpRawResponse, SerpSentCertainty},
};
use geo_worker::{HostOps, SerpCreateRequest, SerpReadMode, SerpReadRequest, SerpReparseRequest};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use uuid::Uuid;

struct Source {
    decoder: DataForSeoSerpSource,
    echo: Mutex<Value>,
    sends: AtomicUsize,
}
impl Source {
    fn new() -> Self {
        Self {
            decoder: DataForSeoSerpSource::new(
                Arc::new(DataForSeoClient::new("synthetic".into(), "synthetic".into()).unwrap()),
                DataForSeoSerpConfig {
                    location_code: 2840,
                    country: "US".into(),
                    city: None,
                    language_code: "en".into(),
                },
            )
            .unwrap(),
            echo: Mutex::new(Value::Null),
            sends: AtomicUsize::new(0),
        }
    }
    fn response(&self, operation: SerpOperation, status: u32) -> SerpRawResponse {
        SerpRawResponse {
            operation, http_status: Some(200), body_complete: true,
            body: serde_json::to_vec(&json!({"status_code":20000,"tasks_count":1,"tasks":[{
                "id":"synthetic-task", "status_code":status, "data":self.echo.lock().unwrap().clone(),
                "result": if status == 20000 { json!([{"items_count":1,"pages_count":1,"items":[{
                    "type":"organic","rank_group":1,"rank_absolute":1,"url":"https://example.org/canary-result","title":"canary-title","page":1
                }]}]) } else { Value::Null }
            }]})).unwrap(),
            sent: SerpSentCertainty::ResponseReceived, error: None,
        }
    }
}
#[async_trait]
impl SerpSource for Source {
    fn protocol(&self, query: &str) -> SerpProtocol {
        self.decoder.protocol(query)
    }
    fn parser_version(&self) -> &str {
        self.decoder.parser_version()
    }
    fn prepare(
        &self,
        measurement: &SerpMeasurement,
        tag: &str,
    ) -> Result<SerpPreparedSubmission, AppError> {
        self.decoder.prepare(measurement, tag)
    }
    async fn send(&self, prepared: &SerpPreparedSubmission) -> SerpRawResponse {
        self.sends.fetch_add(1, Ordering::SeqCst);
        *self.echo.lock().unwrap() =
            serde_json::from_slice::<Value>(&prepared.body).unwrap()[0].clone();
        self.response(SerpOperation::PostTask, 20100)
    }
    fn read_request_sha256(&self, id: &str) -> Result<String, AppError> {
        self.decoder.read_request_sha256(id)
    }
    async fn read(&self, _: &str) -> SerpRawResponse {
        self.response(SerpOperation::GetTask, 20000)
    }
    fn decode_submission(
        &self,
        measurement: &SerpMeasurement,
        raw: &SerpStoredRaw,
        intent: &SerpSendingIntent,
    ) -> Result<String, AppError> {
        self.decoder.decode_submission(measurement, raw, intent)
    }
    fn decode_result(
        &self,
        measurement: &SerpMeasurement,
        raw: &SerpStoredRaw,
        intent: &SerpSendingIntent,
        task_id: &str,
        id: Uuid,
        at: DateTime<Utc>,
    ) -> Result<SerpReadOutcome, AppError> {
        self.decoder
            .decode_result(measurement, raw, intent, task_id, id, at)
    }
    fn verify_recovery(
        &self,
        measurement: &SerpMeasurement,
        raw: &SerpStoredRaw,
        intent: &SerpSendingIntent,
        task_id: &str,
    ) -> Result<(), AppError> {
        self.decoder
            .verify_recovery(measurement, raw, intent, task_id)
    }
}

async fn fixture() -> (
    RepositoryHostOps,
    SerpService,
    Arc<Source>,
    TenantScope,
    AppState,
) {
    let state = AppState::development_with_password("synthetic-tools");
    let tenant = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let project = state
        .project_repository()
        .create(
            &tenant,
            ProjectCreate {
                slug: None,
                display_name: "Synthetic".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(tenant.operator_id, tenant.tenant_id, Some(project.id));
    let source = Arc::new(Source::new());
    let service = SerpService::new(
        Arc::new(MemorySerpRepository::default()),
        state.project_repository(),
        state.question_repository(),
    )
    .with_source(scope.clone(), "synthetic-source".into(), source.clone())
    .unwrap();
    let state = state.with_serp_service(service.clone());
    let ops = RepositoryHostOps::new(Arc::new(MemoryKnowledgeRepository::default()))
        .with_serp_state(state.clone());
    (ops, service, source, scope, state)
}
fn command(at: DateTime<Utc>) -> SerpCreateRequest {
    SerpCreateRequest {
        query: "  canary-query + café%  ".into(),
        idempotency_key: "stable-command".into(),
        scheduled_at: at,
        source_key: None,
        target: None,
        question_reference: None,
    }
}

#[tokio::test]
async fn serp_tools_create_read_and_reparse_use_one_scoped_service() {
    let (ops, service, source, scope, _) = fixture().await;
    let capabilities = ops
        .serp_read(&scope, SerpReadRequest::default())
        .await
        .unwrap();
    assert_eq!(capabilities.capabilities.len(), 1);
    let request = command(capabilities.server_time);
    let created = ops
        .serp_create(&scope, request.clone())
        .await
        .unwrap()
        .measurement;
    assert!(created.details_available);
    assert_eq!(created.query.as_deref(), Some(request.query.as_str()));
    let replay = ops.serp_create(&scope, request).await.unwrap();
    assert_eq!(created.measurement_id, replay.measurement.measurement_id);
    assert_eq!(created.scheduled_at, replay.measurement.scheduled_at);
    service
        .submit_once(&scope, created.measurement_id)
        .await
        .unwrap();
    service
        .poll_once(&scope, created.measurement_id)
        .await
        .unwrap();
    let select = SerpReadRequest {
        mode: SerpReadMode::Detail,
        measurement_id: Some(created.measurement_id),
        ..Default::default()
    };
    let detail = ops.serp_read(&scope, select.clone()).await.unwrap();
    assert_eq!(
        detail.observations[0].results[0].title.as_deref(),
        Some("canary-title")
    );
    let evidence_id = detail.observations[0].evidence_id;
    let sources = ops
        .serp_read(
            &scope,
            SerpReadRequest {
                mode: SerpReadMode::Sources,
                ..select.clone()
            },
        )
        .await
        .unwrap();
    assert!(!sources.evidence.is_empty());
    let serialized = serde_json::to_string(&sources).unwrap();
    for forbidden in [
        "raw_body",
        "password",
        "Authorization",
        "synthetic-task",
        "canary-result",
        "canary-title",
    ] {
        assert!(!serialized.contains(forbidden));
    }
    let reparsed = ops
        .serp_reparse(
            &scope,
            SerpReparseRequest {
                measurement_id: created.measurement_id,
                evidence_id,
                idempotency_key: "parse-once".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(reparsed.observation.evidence_id, evidence_id);
    let replayed_parse = ops
        .serp_reparse(
            &scope,
            SerpReparseRequest {
                measurement_id: created.measurement_id,
                evidence_id,
                idempotency_key: "parse-once".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        reparsed.observation.observation_id,
        replayed_parse.observation.observation_id
    );
    assert_eq!(source.sends.load(Ordering::SeqCst), 1);
    let history = ops
        .serp_read(
            &scope,
            SerpReadRequest {
                mode: SerpReadMode::History,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(history.measurements.len(), 1);
    let other = TenantScope::new(
        scope.operator_id,
        scope.tenant_id,
        Some(Uuid::new_v4().into()),
    );
    assert!(ops.serp_read(&other, select).await.is_err());
}

#[tokio::test]
async fn serp_tools_do_not_guess_multiple_sources_or_unavailable_capabilities() {
    let (ops, service, source, scope, state) = fixture().await;
    let service = service
        .with_source(scope.clone(), "other-source".into(), source)
        .unwrap();
    let multiple = RepositoryHostOps::new(Arc::new(MemoryKnowledgeRepository::default()))
        .with_serp_state(state.with_serp_service(service));
    assert!(
        multiple
            .serp_create(&scope, command(Utc::now()))
            .await
            .is_err()
    );
    let absent = RepositoryHostOps::new(Arc::new(MemoryKnowledgeRepository::default()));
    assert!(
        absent
            .serp_read(&scope, SerpReadRequest::default())
            .await
            .is_err()
    );
    let mut explicit = command(Utc::now());
    explicit.source_key = Some("synthetic-source".into());
    assert!(ops.serp_create(&scope, explicit).await.is_ok());
}
