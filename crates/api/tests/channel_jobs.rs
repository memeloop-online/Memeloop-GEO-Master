use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::SET_COOKIE},
};
use chrono::{Duration, Utc};
use geo_api::{AppState, CSRF_HEADER, router};
use geo_domain::{
    ChannelAccount, ChannelAccountRecord, ChannelOwnerKind, ChannelStatus, DEVELOPMENT_OPERATOR_ID,
    DEVELOPMENT_TENANT_ID, ImportItem, KnowledgePurpose, ProjectCreate, ProjectSettings,
    ProjectStartCommand, ReportAvailability, ReportManifestRef, ReportReduceInput, SourceKind,
    TenantScope, hash_idempotency_key, reduce_report, settings_hash, start_request_hash,
};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

fn request(
    method: &str,
    path: &str,
    cookie: Option<&str>,
    csrf: Option<&str>,
    body: Value,
) -> Request<Body> {
    let selected = if cookie.is_some() {
        format!(
            "{path}{}tenant_id={DEVELOPMENT_TENANT_ID}",
            if path.contains('?') { "&" } else { "?" }
        )
    } else {
        path.to_owned()
    };
    let mut request = Request::builder()
        .method(method)
        .uri(selected)
        .header("host", "localhost:8080")
        .header("origin", "http://localhost:5173")
        .header("content-type", "application/json");
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    if let Some(csrf) = csrf {
        request = request.header(CSRF_HEADER, csrf);
    }
    request.body(Body::from(body.to_string())).unwrap()
}

async fn content(response: axum::response::Response) -> Value {
    serde_json::from_slice(
        &to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap()
}

async fn login(app: &Router) -> (String, String) {
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            None,
            json!({"login_name":"demo@localhost","password":"ledger-test"}),
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
    let body = content(response).await;
    (cookie, body["csrf_token"].as_str().unwrap().to_owned())
}

#[tokio::test]
async fn api_freezes_source_bytes_rejects_uploaded_receipt_and_preserves_report_denominator() {
    let state = AppState::development_with_password("ledger-test");
    let base = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let projects = state.project_repository();
    let project = projects
        .create(
            &base,
            ProjectCreate {
                slug: Some("channel-ledger".into()),
                display_name: "Channel ledger".into(),
                settings: ProjectSettings {
                    brand_name: "Example".into(),
                    market: "CN".into(),
                    language: "en".into(),
                    initial_sources: vec![geo_domain::InitialSource {
                        kind: geo_domain::InitialSourceKind::Text,
                        value: "A public source".into(),
                        visibility: geo_domain::InitialSourceVisibility::Public,
                        version_ref: None,
                        content_hash: None,
                    }],
                    ..ProjectSettings::default()
                },
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(base.operator_id, base.tenant_id, Some(project.id));
    let settings_hash = settings_hash(&project.settings).unwrap();
    let start = projects
        .start(
            &base,
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("channel-ledger"),
                request_hash: start_request_hash(project.id, project.revision, &settings_hash),
                settings_hash,
                operation_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    let imported = state
        .knowledge_repository()
        .import_batch(
            &scope,
            vec![ImportItem {
                client_item_id: "public-source".into(),
                kind: SourceKind::Text,
                name: "A public source".into(),
                purpose: KnowledgePurpose::Public,
                text: Some("Exact approved source text.".into()),
                url: None,
                object_id: None,
                knowledge_release_id: None,
            }],
        )
        .await
        .unwrap();
    let source = imported.items[0].source.as_ref().unwrap();
    let version = imported.items[0].source_version.as_ref().unwrap();
    let account = ChannelAccount {
        account_id: Uuid::new_v4(),
        project_id: project.id,
        owner_kind: ChannelOwnerKind::Customer,
        platform: "zhihu".into(),
        group_id: None,
        status: ChannelStatus::NeedsLogin,
        display_name: None,
        platform_account_id: None,
        avatar_url: None,
        enabled: true,
        proxy_configured: false,
        proxy_server: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    state
        .channel_service()
        .repository
        .save_account(
            &scope,
            ChannelAccountRecord {
                account: account.clone(),
                session: None,
                proxy: None,
            },
        )
        .await
        .unwrap();
    let measure_account = ChannelAccount {
        account_id: Uuid::new_v4(),
        platform: "kimi".into(),
        ..account.clone()
    };
    state
        .channel_service()
        .repository
        .save_account(
            &scope,
            ChannelAccountRecord {
                account: measure_account.clone(),
                session: None,
                proxy: None,
            },
        )
        .await
        .unwrap();
    let app = router(state.clone());
    let (cookie, csrf) = login(&app).await;
    let endpoint = format!(
        "/api/v1/projects/{}/cycles/{}/channel-plan",
        project.id, start.cycle_id
    );
    let planned=app.clone().oneshot(request("POST",&endpoint,Some(&cookie),Some(&csrf),json!({
        "publications":[{"source_id":source.source_id,"source_version_id":version.source_version_id,"platform":"zhihu","account_id":account.account_id}],
        "measurements":[{"account_id":measure_account.account_id,"provider":"kimi","model":"fixed",
            "surface":"consumer_web","search_mode":"web_search","protocol_version":"v1",
            "question_set_version":"q1","question":"What is this?","market":"CN",
            "language":"en","scheduled_at":Utc::now()-Duration::seconds(1),"sample_ordinal":0}]
    }))).await.unwrap();
    let planned_status = planned.status();
    let planned = content(planned).await;
    assert_eq!(planned_status, StatusCode::OK, "{planned}");
    assert_eq!(planned["targets"].as_array().unwrap().len(), 2);
    assert_eq!(
        planned["targets"][0]["input"]["body"],
        "Exact approved source text."
    );
    let target = planned["targets"][0]["target_id"].as_str().unwrap();
    let forged = app
        .clone()
        .oneshot(request(
            "POST",
            &format!(
                "/api/v1/projects/{}/channel-targets/{target}/execute",
                project.id
            ),
            Some(&cookie),
            Some(&csrf),
            json!({"status":"verified","public_url":"https://example.invalid/claimed"}),
        ))
        .await
        .unwrap();
    assert_eq!(forged.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let execution = app
        .clone()
        .oneshot(request(
            "POST",
            &format!(
                "/api/v1/projects/{}/channel-targets/{target}/execute",
                project.id
            ),
            Some(&cookie),
            Some(&csrf),
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(execution.status(), StatusCode::CONFLICT);
    // Missing runner/login capability is a reversible preflight deferral,
    // never a consumed one-shot attempt or a fabricated outcome.
    let view = state
        .channel_job_repository()
        .get_target(&scope, Uuid::parse_str(target).unwrap())
        .await
        .unwrap();
    assert!(view.attempts.is_empty());
    let duplicate = app
        .oneshot(request(
            "POST",
            &format!(
                "/api/v1/projects/{}/channel-targets/{target}/execute",
                project.id
            ),
            Some(&cookie),
            Some(&csrf),
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(duplicate.status(), StatusCode::CONFLICT);
    let cycle = projects
        .get_report_cycle(&scope, project.id, start.cycle_id)
        .await
        .unwrap()
        .unwrap();
    let as_of = std::cmp::max(Utc::now(), cycle.cutoff_at) + Duration::seconds(1);
    let ledger = state
        .channel_job_repository()
        .cycle_inputs(&scope, start.cycle_id, as_of)
        .await
        .unwrap();
    assert_eq!(ledger.manifests[0].expected_count, Some(1));
    assert_eq!(ledger.manifests[1].expected_count, Some(1));
    assert!(ledger.publications.as_ref().unwrap()[0].reason.is_none());
    let input = ReportReduceInput {
        project_id: project.id,
        cycle_id: start.cycle_id,
        report_window_start_at: cycle.report_window_start_at,
        report_window_end_at: cycle.report_window_end_at,
        report_timezone: cycle.report_timezone,
        cutoff_at: cycle.cutoff_at,
        input_temporal_provenance_verified: false,
        input_manifest_versions: cycle
            .document_manifest
            .into_iter()
            .map(|reference| ReportManifestRef {
                kind: geo_domain::ReportManifestKind::Document,
                manifest_id: reference.manifest_id,
                revision: reference.revision,
                sealed: reference.sealed,
                expected_count: reference.expected_count.map(|n| n as u64),
            })
            .chain(ledger.manifests)
            .collect(),
        document_manifest: None,
        publication_targets: ledger.publications,
        measurement_targets: ledger.measurements,
        supplementary_measurements: vec![],
    };
    let report = reduce_report(&scope, &input, 1, None, as_of).unwrap();
    assert_eq!(
        report.publications.availability,
        ReportAvailability::Available
    );
    assert_eq!(report.publications.expected_count, Some(1));
    assert_eq!(report.publications.counts["pending"], 1);
    assert_eq!(report.measurements.expected_count, Some(1));
    assert_eq!(report.measurements.counts["pending"], 1);
    assert!(!report.measurements.counts.contains_key("not_mentioned"));
}
