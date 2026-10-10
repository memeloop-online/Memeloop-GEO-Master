use async_trait::async_trait;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::SET_COOKIE},
};
use chrono::{DateTime, Utc};
use geo_api::{AppState, router};
use geo_domain::{
    AppError, ChannelPlan, ChannelTarget, ChannelTargetInput, DEVELOPMENT_OPERATOR_ID,
    DEVELOPMENT_TENANT_ID, ProjectCreate, ProjectSettings, PublicationLookupCandidate,
    PublicationLookupFinding, PublicationLookupJob, PublicationLookupObservation,
    PublicationLookupRepository, TenantScope,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

struct ReadOnly {
    job: PublicationLookupJob,
    observations: Vec<PublicationLookupObservation>,
}

#[async_trait]
impl PublicationLookupRepository for ReadOnly {
    async fn enqueue(
        &self,
        _: &TenantScope,
        _: Uuid,
        _: Uuid,
        _: DateTime<Utc>,
    ) -> Result<PublicationLookupJob, AppError> {
        panic!("GET enqueued a lookup")
    }
    async fn scan_due(
        &self,
        _: Option<Uuid>,
        _: DateTime<Utc>,
        _: usize,
    ) -> Result<Vec<PublicationLookupCandidate>, AppError> {
        panic!("GET scanned a lookup")
    }
    async fn claim(
        &self,
        _: &TenantScope,
        _: Uuid,
        _: Uuid,
        _: DateTime<Utc>,
        _: DateTime<Utc>,
    ) -> Result<PublicationLookupJob, AppError> {
        panic!("GET claimed a lookup")
    }
    async fn finish(
        &self,
        _: &TenantScope,
        _: Uuid,
        _: PublicationLookupObservation,
        _: Option<DateTime<Utc>>,
    ) -> Result<PublicationLookupJob, AppError> {
        panic!("GET mutated a lookup")
    }
    async fn get(&self, _: &TenantScope, _: Uuid) -> Result<PublicationLookupJob, AppError> {
        Ok(self.job.clone())
    }
    async fn observations(
        &self,
        _: &TenantScope,
        _: Uuid,
    ) -> Result<Vec<PublicationLookupObservation>, AppError> {
        panic!("GET used an unbounded history read")
    }
    async fn observation_page(
        &self,
        _: &TenantScope,
        _: Uuid,
        before: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<PublicationLookupObservation>, AppError> {
        assert_eq!(limit, 20);
        let start = before
            .map(|cursor| {
                self.observations
                    .iter()
                    .position(|item| item.execution_id == cursor)
                    .map(|index| index + 1)
                    .ok_or_else(|| AppError::invalid_request("invalid lookup cursor"))
            })
            .transpose()?
            .unwrap_or(0);
        Ok(self
            .observations
            .iter()
            .skip(start)
            .take(limit + 1)
            .cloned()
            .collect())
    }
}

async fn body(response: axum::response::Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap()
}

#[tokio::test]
async fn lookup_http_enforces_auth_scope_target_kind_and_empty_contract() {
    let state = AppState::development_with_password("lookup-http-test");
    let base = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let projects = state.project_repository();
    let project = projects
        .create(
            &base,
            ProjectCreate {
                slug: None,
                display_name: "Lookup test".into(),
                settings: ProjectSettings {
                    brand_name: "Example".into(),
                    market: "CN".into(),
                    language: "en".into(),
                    ..ProjectSettings::default()
                },
            },
        )
        .await
        .unwrap();
    let other = projects
        .create(
            &base,
            ProjectCreate {
                slug: None,
                display_name: "Other project".into(),
                settings: ProjectSettings {
                    brand_name: "Example".into(),
                    market: "CN".into(),
                    language: "en".into(),
                    ..ProjectSettings::default()
                },
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(base.operator_id, base.tenant_id, Some(project.id));
    let target_id = Uuid::new_v4();
    let measurement_id = Uuid::new_v4();
    let channels = state.channel_job_repository();
    channels
        .create_plan(
            &scope,
            ChannelPlan {
                plan_id: Uuid::new_v4(),
                project_id: project.id,
                cycle_id: Uuid::new_v4(),
                input_hash: "http".into(),
                revision: 1,
                created_at: chrono::Utc::now(),
                targets: vec![
                    ChannelTarget {
                        target_id,
                        input: ChannelTargetInput::Publish {
                            source_id: Uuid::new_v4(),
                            source_version_id: Uuid::new_v4(),
                            platform: "zhihu".into(),
                            account_id: Uuid::new_v4(),
                            title: "Title".into(),
                            body: "Body".into(),
                            body_sha256: "hash".into(),
                        },
                    },
                    ChannelTarget {
                        target_id: measurement_id,
                        input: ChannelTargetInput::Measure {
                            // Use the established measurement contract; this must not
                            // accidentally surface as a publication lookup.
                            provider: "example".into(),
                            model: "example".into(),
                            search_mode: "none".into(),
                            surface: "api".into(),
                            market: "CN".into(),
                            language: "en".into(),
                            account_id: Uuid::new_v4(),
                            protocol_version: "v1".into(),
                            question_set_version: "v1".into(),
                            question: "Example question".into(),
                            scheduled_at: chrono::Utc::now(),
                            sample_ordinal: 1,
                            question_binding: None,
                        },
                    },
                ],
            },
        )
        .await
        .unwrap();
    let app = router(state.clone());
    let login = app
        .clone()
        .oneshot(
            Request::post("/api/v1/auth/login")
                .header("host", "localhost:8080")
                .header("origin", "http://localhost:5173")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"login_name":"demo@localhost","password":"lookup-http-test"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let cookie = login.headers()[SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let get = |path: String, cookie: Option<&str>| {
        let mut request = Request::get(path).header("host", "localhost:8080");
        if let Some(cookie) = cookie {
            request = request.header("cookie", cookie);
        }
        request.body(Body::empty()).unwrap()
    };
    let path = format!(
        "/api/v1/projects/{}/channel-targets/{target_id}/publication-lookup?tenant_id={DEVELOPMENT_TENANT_ID}&project_id={}",
        project.id, project.id,
    );
    assert_eq!(
        app.clone()
            .oneshot(get(path.clone(), None))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let empty = app
        .clone()
        .oneshot(get(path.clone(), Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(empty.status(), StatusCode::OK);
    assert_eq!(
        body(empty).await,
        json!({
            "target_id":target_id, "attempt_id":null, "job":null,
            "observations":[], "next_before":null
        })
    );
    let invalid = app
        .clone()
        .oneshot(get(
            format!("{path}&before={}", Uuid::new_v4()),
            Some(&cookie),
        ))
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    let measurement = app
        .clone()
        .oneshot(get(
            path.replace(&target_id.to_string(), &measurement_id.to_string()),
            Some(&cookie),
        ))
        .await
        .unwrap();
    assert_eq!(measurement.status(), StatusCode::BAD_REQUEST);
    let foreign = app
        .clone()
        .oneshot(get(
            path.replace(&project.id.to_string(), &other.id.to_string()),
            Some(&cookie),
        ))
        .await
        .unwrap();
    assert_eq!(foreign.status(), StatusCode::NOT_FOUND);
    let missing = app
        .oneshot(get(
            path.replace(&target_id.to_string(), &Uuid::new_v4().to_string()),
            Some(&cookie),
        ))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let attempt_id = Uuid::new_v4();
    channels
        .claim(&scope, target_id, attempt_id, Utc::now())
        .await
        .unwrap();
    let input = channels
        .get_target(&scope, target_id)
        .await
        .unwrap()
        .target
        .input;
    let now = Utc::now();
    let evidence = json!({
        "schema_version":"geo.publication.asset_observation.v1",
        "provenance":"live",
        "original_attempt_id":attempt_id,
        "target_id":target_id,
        "account_id":input.account_id(),
        "connector_version":"zhihu.v1",
        "content_sha256":geo_domain::sha256_hex(b"Title\nBody"),
        "observed_at":now,
        "public_url":"https://www.zhihu.com/p/123",
        "secret_field":"private evidence marker"
    });
    let observations = (0..23)
        .map(|number| PublicationLookupObservation {
            execution_id: Uuid::from_u128(100 + number),
            attempt_id,
            finding: PublicationLookupFinding::AssetObserved,
            evidence: evidence.clone(),
            observed_at: now,
            received_at: now,
            error_code: Some("private error marker".into()),
        })
        .collect();
    let read = router(state.with_publication_lookup_repository(Arc::new(ReadOnly {
        job: PublicationLookupJob {
            attempt_id,
            target_id,
            account_id: input.account_id(),
            frozen_input: input,
            connector_version: Some("zhihu.v1".into()),
            candidate_public_url: Some("https://www.zhihu.com/p/123".into()),
            next_due_at: Some(now),
            lease_execution_id: Some(Uuid::new_v4()),
            lease_expires_at: Some(now + chrono::Duration::minutes(1)),
            query_count: 23,
            last_error_code: Some("private job marker".into()),
        },
        observations,
    })));
    let scheduled = read
        .clone()
        .oneshot(get(path.clone(), Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(scheduled.status(), StatusCode::OK);
    assert_eq!(scheduled.headers()["cache-control"], "no-store");
    let scheduled = body(scheduled).await;
    assert_eq!(scheduled["job"]["query_count"], 23);
    assert_eq!(scheduled["job"]["in_progress"], true);
    assert_eq!(scheduled["job"]["last_error_code"], "lookup_error");
    assert_eq!(scheduled["observations"].as_array().unwrap().len(), 20);
    assert_eq!(
        scheduled["observations"][0]["public_url"],
        "https://www.zhihu.com/p/123"
    );
    assert_eq!(scheduled["observations"][0]["error_code"], "lookup_error");
    assert_eq!(scheduled["next_before"], Uuid::from_u128(119).to_string());
    let payload = scheduled.to_string();
    for secret in [
        "private evidence marker",
        "private error marker",
        "private job marker",
        "frozen_input",
        "account_id",
        "lease_execution_id",
        "evidence",
    ] {
        assert!(!payload.contains(secret), "GET leaked {secret}");
    }
    let paged = read
        .clone()
        .oneshot(get(
            format!("{path}&before={}", Uuid::from_u128(119)),
            Some(&cookie),
        ))
        .await
        .unwrap();
    assert_eq!(paged.status(), StatusCode::OK);
    assert_eq!(
        body(paged).await["observations"].as_array().unwrap().len(),
        3
    );
    assert_eq!(
        read.oneshot(get(
            format!("{path}&before={}", Uuid::new_v4()),
            Some(&cookie)
        ))
        .await
        .unwrap()
        .status(),
        StatusCode::BAD_REQUEST
    );
}
