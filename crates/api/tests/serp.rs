//! All responses are injected synthetic data; these tests never invoke a paid source.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use async_trait::async_trait;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::SET_COOKIE},
};
use chrono::{DateTime, Duration, Utc};
use geo_api::{
    AcceptSerpMeasurement, AppState, DataForSeoSerpConfig, DataForSeoSerpSource, ReparseSerpSource,
    SerpPreparedSubmission, SerpReadOutcome, SerpService, SerpSource,
};
use geo_domain::{
    AppError, DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, ProjectCreate, ProjectSettings,
    SerpMeasurement, SerpProtocol, SerpRepository, SerpSendingIntent, SerpStoredRaw, SerpTaskState,
    TenantScope,
};
use geo_persistence::MemorySerpRepository;
use geo_provider::{
    dataforseo::DataForSeoClient,
    serp::{SerpOperation, SerpRawResponse, SerpSentCertainty, SerpTransportError},
};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

struct InjectedSource {
    mapping: DataForSeoSerpSource,
    echo: Mutex<Value>,
    posts: AtomicUsize,
    reads: AtomicUsize,
    pending: AtomicBool,
    unknown_send: AtomicBool,
    malformed_read: AtomicBool,
    cancel_during_send: Mutex<Option<(Arc<MemorySerpRepository>, TenantScope, Uuid)>>,
}

impl InjectedSource {
    fn new() -> Self {
        Self {
            // This client is used only by the pure decoder; send/read below are
            // injected and never call the underlying client's network methods.
            mapping: DataForSeoSerpSource::new(
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
            posts: AtomicUsize::new(0),
            reads: AtomicUsize::new(0),
            pending: AtomicBool::new(false),
            unknown_send: AtomicBool::new(false),
            malformed_read: AtomicBool::new(false),
            cancel_during_send: Mutex::new(None),
        }
    }

    fn response(&self, operation: SerpOperation, status: u32) -> SerpRawResponse {
        let items: Vec<_> = (1..=10)
            .map(|rank| {
                json!({
                    "type":"organic","rank_group":rank,"rank_absolute":rank+1,
                    "url":format!("https://example.org/{rank}"),"title":"Synthetic result","page":1
                })
            })
            .collect();
        SerpRawResponse {
            operation, http_status: Some(200), body_complete: true,
            body: serde_json::to_vec(&json!({
                "status_code":20000,"tasks_count":1,"tasks":[{
                    "id":"synthetic-task","status_code":status,"data":self.echo.lock().unwrap().clone(),
                    "result":if status == 20000 { json!([{"items":items,"pages_count":1,"items_count":10}]) } else { Value::Null }
                }]
            })).unwrap(),
            sent: SerpSentCertainty::ResponseReceived, error: None,
        }
    }
}

#[async_trait]
impl SerpSource for InjectedSource {
    fn protocol(&self, query: &str) -> SerpProtocol {
        self.mapping.protocol(query)
    }
    fn parser_version(&self) -> &str {
        self.mapping.parser_version()
    }
    fn prepare(
        &self,
        measurement: &SerpMeasurement,
        tag: &str,
    ) -> Result<SerpPreparedSubmission, AppError> {
        self.mapping.prepare(measurement, tag)
    }
    async fn send(&self, prepared: &SerpPreparedSubmission) -> SerpRawResponse {
        self.posts.fetch_add(1, Ordering::SeqCst);
        *self.echo.lock().unwrap() =
            serde_json::from_slice::<Value>(&prepared.body).unwrap()[0].clone();
        let cancel = self.cancel_during_send.lock().unwrap().clone();
        if let Some((repository, scope, id)) = cancel {
            repository.cancel(&scope, id, Utc::now()).await.unwrap();
        }
        if self.unknown_send.load(Ordering::SeqCst) {
            return SerpRawResponse {
                operation: SerpOperation::PostTask,
                http_status: None,
                body: vec![],
                body_complete: false,
                sent: SerpSentCertainty::PossiblySent,
                error: Some(SerpTransportError::Timeout),
            };
        }
        self.response(SerpOperation::PostTask, 20100)
    }
    fn read_request_sha256(&self, id: &str) -> Result<String, AppError> {
        self.mapping.read_request_sha256(id)
    }
    async fn read(&self, _: &str) -> SerpRawResponse {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.malformed_read.load(Ordering::SeqCst) {
            return SerpRawResponse {
                operation: SerpOperation::GetTask,
                http_status: Some(200),
                body: b"{".to_vec(),
                body_complete: false,
                sent: SerpSentCertainty::ResponseReceived,
                error: Some(SerpTransportError::BodyReadFailed),
            };
        }
        self.response(
            SerpOperation::GetTask,
            if self.pending.load(Ordering::SeqCst) {
                40602
            } else {
                20000
            },
        )
    }
    fn decode_submission(
        &self,
        measurement: &SerpMeasurement,
        raw: &SerpStoredRaw,
        intent: &SerpSendingIntent,
    ) -> Result<String, AppError> {
        assert!(raw.stored_at >= raw.evidence.captured_at);
        self.mapping.decode_submission(measurement, raw, intent)
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
        assert!(raw.stored_at >= raw.evidence.captured_at);
        self.mapping.decode_result(
            measurement,
            raw,
            intent,
            task_id,
            observation_id,
            analyzed_at,
        )
    }
    fn verify_recovery(
        &self,
        measurement: &SerpMeasurement,
        raw: &SerpStoredRaw,
        intent: &SerpSendingIntent,
        task_id: &str,
    ) -> Result<(), AppError> {
        self.mapping
            .verify_recovery(measurement, raw, intent, task_id)
    }
}

async fn fixture() -> (
    AppState,
    SerpService,
    Arc<MemorySerpRepository>,
    Arc<InjectedSource>,
    TenantScope,
) {
    let state = AppState::development_with_password("serp-test");
    let tenant = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let project = state
        .project_repository()
        .create(
            &tenant,
            ProjectCreate {
                slug: None,
                display_name: "Synthetic search project".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(tenant.operator_id, tenant.tenant_id, Some(project.id));
    let repository = Arc::new(MemorySerpRepository::default());
    let source = Arc::new(InjectedSource::new());
    let service = SerpService::new(
        repository.clone(),
        state.project_repository(),
        state.question_repository(),
    )
    .with_source(scope.clone(), "synthetic-us-en".into(), source.clone())
    .unwrap();
    (state, service, repository, source, scope)
}

fn input(at: DateTime<Utc>) -> AcceptSerpMeasurement {
    AcceptSerpMeasurement {
        idempotency_key: "synthetic-command".into(),
        source_key: "synthetic-us-en".into(),
        query: "  rainfall + café%  ".into(),
        target: None,
        question_reference: None,
        scheduled_at: at,
    }
}

#[tokio::test]
async fn identical_protocols_keep_the_selected_scoped_source_across_restart() {
    let (state, service, repository, first, scope) = fixture().await;
    let second = Arc::new(InjectedSource::new());
    assert_eq!(first.protocol("same"), second.protocol("same"));
    let service = service
        .with_source(scope.clone(), "second-source".into(), second.clone())
        .unwrap();
    let at = Utc::now();
    let mut request = input(at);
    request.source_key = "second-source".into();
    let measurement = service.accept(&scope, request).await.unwrap();
    assert_eq!(measurement.source_key, "second-source");
    // Same idempotency key with a different source cannot replay just because
    // both sources currently advertise the same requested protocol.
    assert!(service.accept(&scope, input(at)).await.is_err());
    let restarted_without_selected = SerpService::new(
        repository.clone(),
        state.project_repository(),
        state.question_repository(),
    )
    .with_source(scope.clone(), "synthetic-us-en".into(), first.clone())
    .unwrap();
    assert!(
        restarted_without_selected
            .submit_once(&scope, measurement.measurement_id)
            .await
            .is_err()
    );
    assert_eq!(first.posts.load(Ordering::SeqCst), 0);
    let restarted = restarted_without_selected
        .with_source(scope.clone(), "second-source".into(), second.clone())
        .unwrap();
    restarted
        .submit_once(&scope, measurement.measurement_id)
        .await
        .unwrap();
    restarted
        .poll_once(&scope, measurement.measurement_id)
        .await
        .unwrap();
    assert_eq!(first.posts.load(Ordering::SeqCst), 0);
    assert_eq!(first.reads.load(Ordering::SeqCst), 0);
    assert_eq!(second.posts.load(Ordering::SeqCst), 1);
    assert_eq!(second.reads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn accepts_exact_query_and_posts_once_after_persisted_intent() {
    let (_, service, repository, source, scope) = fixture().await;
    let at = Utc::now() - Duration::minutes(45);
    let measurement = service.accept(&scope, input(at)).await.unwrap();
    let replay = service.accept(&scope, input(at)).await.unwrap();
    assert_eq!(measurement.measurement_id, replay.measurement_id);
    assert_eq!(measurement.protocol.query, "  rainfall + café%  ");
    let mut changed = input(at);
    changed.query = "different query".into();
    assert!(service.accept(&scope, changed).await.is_err());
    let (left, right) = tokio::join!(
        service.submit_once(&scope, measurement.measurement_id),
        service.submit_once(&scope, measurement.measurement_id),
    );
    left.unwrap();
    right.unwrap();
    assert_eq!(source.posts.load(Ordering::SeqCst), 1);
    assert_eq!(
        repository
            .get(&scope, measurement.measurement_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        SerpTaskState::AwaitingResult
    );
    assert_eq!(
        source.echo.lock().unwrap()["keyword"],
        "  rainfall %2B café%25  "
    );
    service
        .poll_once(&scope, measurement.measurement_id)
        .await
        .unwrap();
    let detail = service
        .detail(&scope, measurement.measurement_id, None, 20)
        .await
        .unwrap();
    assert_eq!(detail.measurement.state, SerpTaskState::Completed);
    assert_eq!(detail.observations.len(), 1);
    assert_eq!(detail.observations[0].results.len(), 10);
    assert_eq!(detail.observations[0].coverage.observed_organic_depth, 10);
    assert!(detail.observations[0].actual_conditions.country.is_none());
    let sources = service
        .sources(&scope, measurement.measurement_id, None, 20)
        .await
        .unwrap();
    assert_eq!(sources.items.len(), 2);
    let evidence_id = detail.observations[0].raw_evidence_id;
    let raw = service
        .raw(&scope, measurement.measurement_id, evidence_id)
        .await
        .unwrap();
    assert_eq!(
        raw.evidence.response_sha256,
        detail.observations[0].raw_sha256
    );
    let original = detail.observations[0].clone();
    let reparse = service
        .reparse(
            &scope,
            measurement.measurement_id,
            ReparseSerpSource {
                evidence_id,
                idempotency_key: "parse-again".into(),
            },
        )
        .await
        .unwrap();
    assert_ne!(reparse.observation_id, original.observation_id);
    let replay = service
        .reparse(
            &scope,
            measurement.measurement_id,
            ReparseSerpSource {
                evidence_id,
                idempotency_key: "parse-again".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(reparse, replay);
    let detail = service
        .detail(&scope, measurement.measurement_id, None, 20)
        .await
        .unwrap();
    assert_eq!(detail.observations.len(), 2);
    assert!(detail.observations.contains(&original));
    assert_eq!(source.posts.load(Ordering::SeqCst), 1);
    assert_eq!(source.reads.load(Ordering::SeqCst), 1);
    assert!(
        service
            .reparse(
                &scope,
                measurement.measurement_id,
                ReparseSerpSource {
                    evidence_id: Uuid::new_v4(),
                    idempotency_key: "parse-again".into(),
                }
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn provider_queue_is_persistently_scheduled_not_a_timeout_or_busy_loop() {
    let (_, service, repository, source, scope) = fixture().await;
    source.pending.store(true, Ordering::SeqCst);
    let measurement = service
        .accept(&scope, input(Utc::now() - Duration::minutes(45)))
        .await
        .unwrap();
    service
        .submit_once(&scope, measurement.measurement_id)
        .await
        .unwrap();
    service
        .poll_once(&scope, measurement.measurement_id)
        .await
        .unwrap();
    let execution = repository
        .get_execution(&scope, measurement.measurement_id)
        .await
        .unwrap()
        .unwrap();
    assert!(execution.claim.is_none());
    assert!(execution.next_poll_at.unwrap() > Utc::now());
    service
        .poll_once(&scope, measurement.measurement_id)
        .await
        .unwrap();
    service.dispatch_due_page(&scope, None, 20).await.unwrap();
    assert_eq!(source.posts.load(Ordering::SeqCst), 1);
    assert_eq!(source.reads.load(Ordering::SeqCst), 1);
    assert_eq!(
        service
            .detail(&scope, measurement.measurement_id, None, 20)
            .await
            .unwrap()
            .measurement
            .state,
        SerpTaskState::AwaitingResult
    );
}

#[tokio::test]
async fn unknown_send_and_cancellation_never_authorize_another_post() {
    let (_, service, repository, source, scope) = fixture().await;
    source.unknown_send.store(true, Ordering::SeqCst);
    let measurement = service.accept(&scope, input(Utc::now())).await.unwrap();
    service
        .submit_once(&scope, measurement.measurement_id)
        .await
        .unwrap();
    service
        .submit_once(&scope, measurement.measurement_id)
        .await
        .unwrap();
    assert_eq!(source.posts.load(Ordering::SeqCst), 1);
    assert_eq!(
        repository
            .get(&scope, measurement.measurement_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        SerpTaskState::Unknown
    );
    service
        .cancel(&scope, measurement.measurement_id)
        .await
        .unwrap();
    service
        .submit_once(&scope, measurement.measurement_id)
        .await
        .unwrap();
    assert_eq!(source.posts.load(Ordering::SeqCst), 1);
    assert_eq!(
        repository
            .get(&scope, measurement.measurement_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        SerpTaskState::Cancelled
    );
}

#[tokio::test]
async fn exact_task_recovery_is_read_only_and_cancelled_late_response_is_archived() {
    let (_, service, repository, source, scope) = fixture().await;
    let measurement = service.accept(&scope, input(Utc::now())).await.unwrap();
    service
        .submit_once(&scope, measurement.measurement_id)
        .await
        .unwrap();
    source.malformed_read.store(true, Ordering::SeqCst);
    service
        .poll_once(&scope, measurement.measurement_id)
        .await
        .unwrap();
    assert_eq!(
        service
            .detail(&scope, measurement.measurement_id, None, 20)
            .await
            .unwrap()
            .measurement
            .state,
        SerpTaskState::Unknown
    );
    let sources = service
        .sources(&scope, measurement.measurement_id, None, 20)
        .await
        .unwrap();
    assert!(sources.items.iter().any(|raw| !raw.body_complete));
    source.malformed_read.store(false, Ordering::SeqCst);
    service
        .recover_once(&scope, measurement.measurement_id)
        .await
        .unwrap();
    service
        .poll_once(&scope, measurement.measurement_id)
        .await
        .unwrap();
    assert_eq!(
        service
            .detail(&scope, measurement.measurement_id, None, 20)
            .await
            .unwrap()
            .measurement
            .state,
        SerpTaskState::Completed
    );
    assert_eq!(source.posts.load(Ordering::SeqCst), 1);

    let mut second = input(Utc::now());
    second.idempotency_key = "cancel-race".into();
    let second = service.accept(&scope, second).await.unwrap();
    *source.cancel_during_send.lock().unwrap() =
        Some((repository, scope.clone(), second.measurement_id));
    assert!(
        service
            .submit_once(&scope, second.measurement_id)
            .await
            .is_err()
    );
    let sources = service
        .sources(&scope, second.measurement_id, None, 20)
        .await
        .unwrap();
    assert_eq!(sources.items.len(), 1);
    service
        .recover_once(&scope, second.measurement_id)
        .await
        .unwrap();
    service
        .poll_once(&scope, second.measurement_id)
        .await
        .unwrap();
    assert_eq!(
        service
            .detail(&scope, second.measurement_id, None, 20)
            .await
            .unwrap()
            .measurement
            .state,
        SerpTaskState::Cancelled
    );
    assert_eq!(source.posts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn route_capability_and_history_are_scoped_without_global_source_access() {
    let (state, service, repository, _, scope) = fixture().await;
    let tenant = TenantScope::new(scope.operator_id, scope.tenant_id, None);
    let other = state
        .project_repository()
        .create(
            &tenant,
            ProjectCreate {
                slug: None,
                display_name: "Other synthetic project".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    let other = TenantScope::new(scope.operator_id, scope.tenant_id, Some(other.id));
    assert!(service.capabilities(&other).is_empty());
    assert!(service.accept(&other, input(Utc::now())).await.is_err());
    let measurement = service.accept(&scope, input(Utc::now())).await.unwrap();
    assert!(
        service
            .detail(&other, measurement.measurement_id, None, 20)
            .await
            .is_err()
    );
    let unconfigured = SerpService::new(
        repository,
        state.project_repository(),
        state.question_repository(),
    );
    assert!(unconfigured.capabilities(&scope).is_empty());
    assert_eq!(
        unconfigured
            .detail(&scope, measurement.measurement_id, None, 20)
            .await
            .unwrap()
            .measurement,
        measurement
    );
    assert!(
        unconfigured
            .submit_once(&scope, measurement.measurement_id)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn question_binding_is_resolved_from_immutable_project_revision() {
    use geo_domain::{
        CreateQuestionSet, QuestionDraft, QuestionReference, QuestionSource, QuestionSourceKind,
    };
    let (state, service, _, _, scope) = fixture().await;
    let at = Utc::now();
    let request = input(at);
    let version = state
        .question_repository()
        .create_set(
            &scope,
            CreateQuestionSet {
                idempotency_key: "search-question-set".into(),
                name: "Synthetic questions".into(),
                questions: vec![QuestionDraft {
                    question_id: None,
                    text: request.query.clone(),
                    intent: "research".into(),
                    product_refs: vec![],
                    market: "US".into(),
                    language: "en".into(),
                    source: QuestionSource {
                        kind: QuestionSourceKind::UserProvided,
                        reference_id: None,
                    },
                    weight: 1,
                }],
            },
        )
        .await
        .unwrap();
    let reference = QuestionReference {
        question_set_id: version.question_set_id,
        question_set_version_id: version.id,
        question_id: version.questions[0].question_id,
        question_revision_id: version.questions[0].id,
    };
    let expected = state
        .question_repository()
        .resolve_question(&scope, reference)
        .await
        .unwrap()
        .binding;
    let mut request = input(at);
    request.question_reference = Some(reference);
    let measurement = service.accept(&scope, request).await.unwrap();
    assert_eq!(measurement.question_binding, Some(expected));
    let mut wrong = input(at);
    wrong.idempotency_key = "wrong-question".into();
    wrong.question_reference = Some(reference);
    wrong.query = "changed text".into();
    assert!(service.accept(&scope, wrong).await.is_err());
    let mut wrong = input(at);
    wrong.idempotency_key = "wrong-revision".into();
    wrong.question_reference = Some(QuestionReference {
        question_revision_id: Uuid::new_v4(),
        ..reference
    });
    assert!(service.accept(&scope, wrong).await.is_err());
}

fn http_request(
    method: &str,
    path: &str,
    cookie: Option<&str>,
    csrf: Option<&str>,
    body: Value,
) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(format!(
            "{path}{}tenant_id={DEVELOPMENT_TENANT_ID}",
            if path.contains('?') { "&" } else { "?" }
        ))
        .header("host", "localhost:8080")
        .header("origin", "http://localhost:5173")
        .header("content-type", "application/json");
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    if let Some(csrf) = csrf {
        request = request.header(geo_api::CSRF_HEADER, csrf);
    }
    request.body(Body::from(body.to_string())).unwrap()
}

async fn http_body(response: axum::response::Response) -> Value {
    serde_json::from_slice(
        &to_bytes(response.into_body(), 8 * 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn http_uses_same_resource_and_gets_never_dispatch_or_leak_authorization() {
    let (state, service, _, source, scope) = fixture().await;
    let app = geo_api::router(state.with_serp_service(service.clone()));
    let base = format!(
        "/api/v1/projects/{}/serp-measurements",
        scope.project_id.unwrap()
    );
    let unauthenticated = app
        .clone()
        .oneshot(http_request("GET", &base, None, None, Value::Null))
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    let response = app
        .clone()
        .oneshot(http_request(
            "POST",
            "/api/v1/auth/login",
            None,
            None,
            json!({"login_name":"demo@localhost","password":"serp-test"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()[SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let csrf = http_body(response).await["csrf_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let body = serde_json::to_value(input(Utc::now())).unwrap();
    let denied = app
        .clone()
        .oneshot(http_request(
            "POST",
            &base,
            Some(&cookie),
            None,
            body.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    let mut forged = body.clone();
    forged["question_binding"] = json!({"purpose":"optimization"});
    let denied = app
        .clone()
        .oneshot(http_request(
            "POST",
            &base,
            Some(&cookie),
            Some(&csrf),
            forged,
        ))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let response = app
        .clone()
        .oneshot(http_request(
            "POST",
            &base,
            Some(&cookie),
            Some(&csrf),
            body.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let accepted = http_body(response).await;
    let id: Uuid = serde_json::from_value(accepted["measurement_id"].clone()).unwrap();
    let replay = app
        .clone()
        .oneshot(http_request(
            "POST",
            &base,
            Some(&cookie),
            Some(&csrf),
            body,
        ))
        .await
        .unwrap();
    assert_eq!(
        http_body(replay).await["measurement_id"],
        accepted["measurement_id"]
    );
    assert_eq!(source.posts.load(Ordering::SeqCst), 0);
    service.dispatch_due_page(&scope, None, 20).await.unwrap();
    service.dispatch_due_page(&scope, None, 20).await.unwrap();
    let response = app
        .clone()
        .oneshot(http_request(
            "GET",
            &format!("{base}/{id}"),
            Some(&cookie),
            None,
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let detail = http_body(response).await;
    assert_eq!(detail["measurement"]["state"], "completed");
    assert_eq!(detail["observations"].as_array().unwrap().len(), 1);
    assert!(detail["execution"].get("send_token").is_none());
    assert!(detail["execution"].get("claim_token").is_none());
    let evidence_id = detail["observations"][0]["raw_evidence_id"]
        .as_str()
        .unwrap();
    let response = app
        .clone()
        .oneshot(http_request(
            "GET",
            &format!("{base}/{id}/raw/{evidence_id}"),
            Some(&cookie),
            None,
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let raw = http_body(response).await;
    assert_eq!(raw["evidence"]["measurement_id"], json!(id));
    let response = app
        .oneshot(http_request(
            "POST",
            &format!("{base}/{id}/reparse"),
            Some(&cookie),
            Some(&csrf),
            json!({"evidence_id":evidence_id,"idempotency_key":"http-reparse"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(source.posts.load(Ordering::SeqCst), 1);
    assert_eq!(source.reads.load(Ordering::SeqCst), 1);
}
