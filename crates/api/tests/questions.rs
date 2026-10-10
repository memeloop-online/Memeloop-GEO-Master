use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::SET_COOKIE},
};
use chrono::Utc;
use geo_api::{
    AppState, CSRF_HEADER, EventBus, MemoryIdempotencyStore, MemoryOperationStore, router,
};
use geo_domain::{
    ChannelAccount, ChannelAccountRecord, ChannelOwnerKind, ChannelStatus, CreateQuestionSet,
    DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, ImportItem, KnowledgePurpose, Membership,
    MemoryAuthRepository, MemoryKnowledgeRepository, MemoryProjectRepository, ProjectCreate,
    ProjectSettings, ProjectStartCommand, QuestionDraft, QuestionSource, QuestionSourceKind, Role,
    SourceKind, TenantScope, User, hash_idempotency_key, settings_hash, start_request_hash,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

fn request(
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    csrf: Option<&str>,
    body: Value,
) -> Request<Body> {
    let uri = if cookie.is_some() {
        format!(
            "{uri}{}tenant_id={DEVELOPMENT_TENANT_ID}",
            if uri.contains('?') { "&" } else { "?" }
        )
    } else {
        uri.into()
    };
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost:8080")
        .header("origin", "http://localhost:5173")
        .header("content-type", "application/json");
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    if let Some(csrf) = csrf {
        builder = builder.header(CSRF_HEADER, csrf);
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

async fn body(response: axum::response::Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap()).unwrap()
}

async fn login(app: &Router, user: &str, password: &str) -> (String, String) {
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            None,
            json!({"login_name":user,"password":password}),
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
    let csrf = body(response).await["csrf_token"]
        .as_str()
        .unwrap()
        .to_owned();
    (cookie, csrf)
}

fn draft(text: &str) -> QuestionDraft {
    QuestionDraft {
        question_id: None,
        text: text.into(),
        intent: "research".into(),
        product_refs: vec![],
        market: "CN".into(),
        language: "en".into(),
        source: QuestionSource {
            kind: QuestionSourceKind::UserProvided,
            reference_id: None,
        },
        weight: 1,
    }
}

#[tokio::test]
async fn source_backed_create_and_revise_replay_after_source_withdrawal() {
    let auth = Arc::new(MemoryAuthRepository::development_with_password(
        "question-password",
    ));
    let projects = Arc::new(MemoryProjectRepository::default());
    let knowledge = Arc::new(MemoryKnowledgeRepository::default());
    let state = AppState::with_stores_and_auth_and_projects_and_knowledge(
        Arc::new(MemoryOperationStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
        auth.clone(),
        projects.clone(),
        knowledge,
        EventBus::default(),
        false,
    );
    let base = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let project = state
        .project_repository()
        .create(
            &base,
            ProjectCreate {
                slug: Some("source-replay-questions".into()),
                display_name: "Source replay".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(base.operator_id, base.tenant_id, Some(project.id));
    let imported = state
        .knowledge_repository()
        .import_batch(
            &scope,
            vec![ImportItem {
                client_item_id: "public-faq".into(),
                kind: SourceKind::Text,
                name: "Synthetic FAQ".into(),
                purpose: KnowledgePurpose::Public,
                text: Some("Synthetic public answers.".into()),
                url: None,
                object_id: None,
                knowledge_release_id: None,
            }],
        )
        .await
        .unwrap();
    let source_id = imported.items[0].source.as_ref().unwrap().source_id;
    let endpoint = format!("/api/v1/projects/{}/question-sets", project.id);
    let create = json!({
        "idempotency_key":"source-create","name":"FAQ",
        "questions":[{"text":"What is covered?","intent":"research","product_refs":[],"market":"CN","language":"en","source":{"kind":"faq","reference_id":source_id},"weight":1}]
    });
    let app = router(state.clone());
    let (cookie, csrf) = login(&app, "demo@localhost", "question-password").await;
    let first_response = app
        .clone()
        .oneshot(request(
            "POST",
            &endpoint,
            Some(&cookie),
            Some(&csrf),
            create.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);
    let first = body(first_response).await;
    let versions = format!(
        "{endpoint}/{}/versions",
        first["question_set_id"].as_str().unwrap()
    );
    let revise = json!({
        "idempotency_key":"source-revise",
        "base_version_id":first["id"],
        "name":"FAQ revised",
        "questions":[{
            "question_id":first["questions"][0]["question_id"],
            "text":"What is covered now?",
            "intent":"research","product_refs":[],"market":"CN","language":"en",
            "source":{"kind":"faq","reference_id":source_id},"weight":1
        }]
    });
    let revised_response = app
        .clone()
        .oneshot(request(
            "POST",
            &versions,
            Some(&cookie),
            Some(&csrf),
            revise.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(revised_response.status(), StatusCode::OK);
    let revised = body(revised_response).await;

    // The knowledge adapter now reports no source, as after withdrawal. The
    // same persisted question ledger/auth/project remain available. No source
    // is reintroduced or quietly treated as currently public.
    let withdrawn = AppState::with_stores_and_auth_and_projects_and_knowledge(
        Arc::new(MemoryOperationStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
        auth,
        projects,
        Arc::new(MemoryKnowledgeRepository::default()),
        EventBus::default(),
        false,
    )
    .with_question_repository(state.question_repository());
    assert!(
        withdrawn
            .knowledge_repository()
            .get_source(&scope, source_id)
            .await
            .unwrap()
            .is_none()
    );
    let app = router(withdrawn);
    let create_replay = app
        .clone()
        .oneshot(request(
            "POST",
            &endpoint,
            Some(&cookie),
            Some(&csrf),
            create.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(create_replay.status(), StatusCode::OK);
    assert_eq!(body(create_replay).await, first);
    let revise_replay = app
        .clone()
        .oneshot(request(
            "POST",
            &versions,
            Some(&cookie),
            Some(&csrf),
            revise.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(revise_replay.status(), StatusCode::OK);
    assert_eq!(body(revise_replay).await, revised);
    let mut changed_same_key = create.clone();
    changed_same_key["questions"][0]["text"] = json!("Changed after withdrawal");
    assert_eq!(
        app.clone()
            .oneshot(request(
                "POST",
                &endpoint,
                Some(&cookie),
                Some(&csrf),
                changed_same_key
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    let mut new_write = create;
    new_write["idempotency_key"] = json!("new-request-after-withdrawal");
    assert_ne!(
        app.clone()
            .oneshot(request(
                "POST",
                &endpoint,
                Some(&cookie),
                Some(&csrf),
                new_write
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn scoped_question_http_versions_idempotency_writer_and_reference_guards() {
    let auth = Arc::new(MemoryAuthRepository::development_with_password(
        "question-password",
    ));
    let state = AppState::with_stores_and_auth_and_projects(
        Arc::new(MemoryOperationStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
        auth.clone(),
        Arc::new(geo_domain::MemoryProjectRepository::default()),
        EventBus::default(),
        false,
    );
    let base = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let project = state
        .project_repository()
        .create(
            &base,
            ProjectCreate {
                slug: Some("questions-alpha".into()),
                display_name: "Question project".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    let other = state
        .project_repository()
        .create(
            &base,
            ProjectCreate {
                slug: Some("questions-beta".into()),
                display_name: "Other project".into(),
                settings: ProjectSettings {
                    brand_name: "Example".into(),
                    market: "CN".into(),
                    language: "en".into(),
                    initial_sources: vec![geo_domain::InitialSource {
                        kind: geo_domain::InitialSourceKind::Text,
                        value: "Synthetic test description".into(),
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
    let app = router(state.clone());
    let (cookie, csrf) = login(&app, "demo@localhost", "question-password").await;
    let endpoint = format!("/api/v1/projects/{}/question-sets", project.id);
    let command = json!({
        "idempotency_key":"question-request-1","name":"Baseline",
        "questions":[draft("Which product?"),draft("Where is it available?")]
    });
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            &endpoint,
            Some(&cookie),
            Some(&csrf),
            command.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let first = body(created).await;
    assert_eq!(first["questions"].as_array().unwrap().len(), 2);
    assert_eq!(first["evaluation_count"], 1);
    let replay = body(
        app.clone()
            .oneshot(request(
                "POST",
                &endpoint,
                Some(&cookie),
                Some(&csrf),
                command.clone(),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(replay["id"], first["id"]);
    let mut changed = command.clone();
    changed["name"] = json!("Different payload");
    assert_eq!(
        app.clone()
            .oneshot(request(
                "POST",
                &endpoint,
                Some(&cookie),
                Some(&csrf),
                changed
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    let set_id = first["question_set_id"].as_str().unwrap();
    let old_id = first["id"].as_str().unwrap();
    let versions = format!("{endpoint}/{set_id}/versions");
    let detail = body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("{versions}/{old_id}"),
                Some(&cookie),
                None,
                json!({}),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(detail, first);
    let listed = body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("{endpoint}?limit=1"),
                Some(&cookie),
                None,
                json!({}),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(listed["items"][0]["id"], set_id);
    let version_list = body(
        app.clone()
            .oneshot(request("GET", &versions, Some(&cookie), None, json!({})))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(version_list["items"][0]["id"], old_id);
    let question = &first["questions"][0];
    let reference = geo_domain::QuestionReference {
        question_set_id: set_id.parse().unwrap(),
        question_set_version_id: old_id.parse().unwrap(),
        question_id: question["question_id"].as_str().unwrap().parse().unwrap(),
        question_revision_id: question["id"].as_str().unwrap().parse().unwrap(),
    };
    let resolved = state
        .question_repository()
        .resolve_question(&scope, reference)
        .await
        .unwrap();
    assert_eq!(resolved.revision.text, "Which product?");
    let mut wrong = reference;
    wrong.question_revision_id = first["questions"][1]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        state
            .question_repository()
            .resolve_question(&scope, wrong)
            .await
            .is_err()
    );
    let foreign_scope = TenantScope::new(base.operator_id, base.tenant_id, Some(other.id));
    assert!(
        state
            .question_repository()
            .resolve_question(&foreign_scope, reference)
            .await
            .is_err()
    );
    let foreign = format!(
        "/api/v1/projects/{}/question-sets/{set_id}/versions/{old_id}",
        other.id
    );
    assert_eq!(
        app.clone()
            .oneshot(request("GET", &foreign, Some(&cookie), None, json!({})))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let forged = json!({"idempotency_key":"forged-fields","name":"No","questions":[{"text":"Question?","intent":"research","product_refs":[Uuid::new_v4()],"market":"CN","language":"en","source":{"kind":"user_provided"},"weight":1,"purpose":"optimization"}]});
    assert_ne!(
        app.clone()
            .oneshot(request(
                "POST",
                &endpoint,
                Some(&cookie),
                Some(&csrf),
                forged
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let unknown_product = json!({"idempotency_key":"unknown-product","name":"No","questions":[{"text":"Question?","intent":"research","product_refs":[Uuid::new_v4()],"market":"CN","language":"en","source":{"kind":"user_provided"},"weight":1}]});
    assert_ne!(
        app.clone()
            .oneshot(request(
                "POST",
                &endpoint,
                Some(&cookie),
                Some(&csrf),
                unknown_product
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let unknown_source = json!({"idempotency_key":"unknown-source","name":"No","questions":[{"text":"Question?","intent":"research","product_refs":[],"market":"CN","language":"en","source":{"kind":"faq","reference_id":Uuid::new_v4()},"weight":1}]});
    assert_ne!(
        app.clone()
            .oneshot(request(
                "POST",
                &endpoint,
                Some(&cookie),
                Some(&csrf),
                unknown_source
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    let viewer = User::new(
        Uuid::new_v4().into(),
        DEVELOPMENT_OPERATOR_ID,
        "questions-viewer@localhost",
        "Viewer",
        "viewer-password",
    )
    .unwrap();
    auth.insert_user(viewer.clone()).await.unwrap();
    auth.insert_membership(Membership::new(
        viewer.id,
        DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Role::CustomerReadOnly,
    ))
    .await
    .unwrap();
    let (viewer_cookie, viewer_csrf) =
        login(&app, "questions-viewer@localhost", "viewer-password").await;
    assert_eq!(
        app.clone()
            .oneshot(request(
                "GET",
                &endpoint,
                Some(&viewer_cookie),
                None,
                json!({})
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(app.clone().oneshot(request("POST", &versions, Some(&viewer_cookie), Some(&viewer_csrf), json!({"idempotency_key":"read-only","base_version_id":old_id,"name":"Changed","questions":[]}))).await.unwrap().status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn bound_plan_derives_immutable_question_and_rejects_forged_or_cross_project_reference() {
    let state = AppState::development_with_password("question-password");
    let base = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let project = state
        .project_repository()
        .create(
            &base,
            ProjectCreate {
                slug: Some("bound-question-plan".into()),
                display_name: "Bound plan".into(),
                settings: ProjectSettings {
                    brand_name: "Example".into(),
                    market: "CN".into(),
                    language: "en".into(),
                    initial_sources: vec![geo_domain::InitialSource {
                        kind: geo_domain::InitialSourceKind::Text,
                        value: "Synthetic test description".into(),
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
    let settings_digest = settings_hash(&project.settings).unwrap();
    let started = state
        .project_repository()
        .start(
            &base,
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("bound-question-plan"),
                request_hash: start_request_hash(project.id, project.revision, &settings_digest),
                settings_hash: settings_digest,
                operation_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    let version = state
        .question_repository()
        .create_set(
            &scope,
            CreateQuestionSet {
                idempotency_key: "bound-questions".into(),
                name: "Baseline".into(),
                questions: vec![draft("Freeze this text?")],
            },
        )
        .await
        .unwrap();
    let q = &version.questions[0];
    let reference = geo_domain::QuestionReference {
        question_set_id: version.question_set_id,
        question_set_version_id: version.id,
        question_id: q.question_id,
        question_revision_id: q.id,
    };
    let account = ChannelAccount {
        account_id: Uuid::new_v4(),
        project_id: project.id,
        owner_kind: ChannelOwnerKind::Customer,
        platform: "kimi".into(),
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
    let app = router(state.clone());
    let (cookie, csrf) = login(&app, "demo@localhost", "question-password").await;
    let endpoint = format!(
        "/api/v1/projects/{}/cycles/{}/channel-plan",
        project.id, started.cycle_id
    );
    let bound = json!({"account_id":account.account_id,"provider":"kimi","model":"fixed","surface":"consumer_web","search_mode":"web_search","protocol_version":"v1","question":reference,"scheduled_at":Utc::now(),"sample_ordinal":0});
    let plan = json!({"publications":[],"measurements":[],"bound_measurements":[bound.clone()]});
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            &endpoint,
            Some(&cookie),
            Some(&csrf),
            plan.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let frozen = body(response).await;
    assert_eq!(frozen["targets"].as_array().unwrap().len(), 1);
    let frozen_json = &frozen["targets"][0]["input"];
    assert_eq!(frozen_json["question"], q.text);
    assert_eq!(frozen_json["market"], q.market);
    assert_eq!(frozen_json["language"], q.language);
    assert_eq!(
        frozen_json["question_binding"]["purpose"],
        "frozen_evaluation"
    );
    assert_eq!(frozen_json["question_set_version"], version.id.to_string());
    let replay = body(
        app.clone()
            .oneshot(request("POST", &endpoint, Some(&cookie), Some(&csrf), plan))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(replay, frozen);
    let mut forged = bound.clone();
    forged["purpose"] = json!("optimization");
    assert_ne!(
        app.clone()
            .oneshot(request(
                "POST",
                &endpoint,
                Some(&cookie),
                Some(&csrf),
                json!({"publications":[],"measurements":[],"bound_measurements":[forged]})
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let mut wrong = bound.clone();
    wrong["question"]["question_id"] = json!(Uuid::new_v4());
    assert_ne!(
        app.clone()
            .oneshot(request(
                "POST",
                &endpoint,
                Some(&cookie),
                Some(&csrf),
                json!({"publications":[],"measurements":[],"bound_measurements":[wrong]})
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let wrong_scope = TenantScope::new(
        base.operator_id,
        base.tenant_id,
        Some(geo_domain::ProjectId::new(Uuid::new_v4())),
    );
    assert!(
        state
            .question_repository()
            .resolve_question(&wrong_scope, reference)
            .await
            .is_err()
    );
}
