use chrono::{Duration, Utc};
use geo_domain::{
    ChannelJobRepository, ChannelOutcome, ChannelOutcomeStatus, ChannelTarget, ChannelTargetInput,
    ErrorCode, ObservationAnalysisOutcome, ObservationAnalysisRepository,
    ObservationAnalysisRequest, ObservationAnalysisResult, ObservationAnalysisSource,
    ObservationAnalysisState, ObservationCaptureInput, ObservationCaptureRepository,
    ObservationCaptureSnapshot, StandaloneMeasurementPlan, TenantScope, sha256_hex,
};
use geo_persistence::{
    Database, DatabaseConfig, PgChannelJobRepository, PgObservationAnalysisRepository,
    PgObservationCaptureRepository,
};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn analysis_intent_claim_terminal_replay_and_original_evidence_remain_scoped() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable PostgreSQL URL required");
    let config = DatabaseConfig::from_url(url).expect("valid database URL");
    let database = Database::connect_and_migrate(&config)
        .await
        .expect("migrations");
    let (operator, tenant, project, other_project) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'Synthetic')")
        .bind(operator)
        .bind(format!("analysis-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,'Synthetic')")
        .bind(tenant).bind(operator).bind(format!("analysis-{tenant}")).execute(database.pool()).await.unwrap();
    for id in [project, other_project] {
        sqlx::query("INSERT INTO projects (project_id,operator_id,tenant_id,slug,display_name) VALUES ($1,$2,$3,$4,'Synthetic')")
            .bind(id).bind(operator).bind(tenant).bind(format!("analysis-{id}")).execute(database.pool()).await.unwrap();
    }
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.into()));
    let other = TenantScope::new(operator.into(), tenant.into(), Some(other_project.into()));
    let jobs = PgChannelJobRepository::from_database(&database);
    let captures = PgObservationCaptureRepository::from_database(&database);
    let store = PgObservationAnalysisRepository::from_database(&database);
    let (target_id, attempt_id, account_id, capture_id) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    let start = chrono::DateTime::from_timestamp(Utc::now().timestamp() - 10, 0).unwrap();
    let observed_at = start + Duration::seconds(1);
    let received_at = start + Duration::seconds(2);
    let created_at = start + Duration::seconds(3);
    let source_json = r#"{"messages":[{"content":"Synthetic saved response"}]}"#.to_owned();
    let source_sha256 = sha256_hex(source_json.as_bytes());
    let plan_id = Uuid::new_v4();
    jobs.create_measurement_plan(
        &scope,
        &format!("plan-{plan_id}"),
        "synthetic-request",
        StandaloneMeasurementPlan {
            plan_id,
            project_id: scope.project_id.unwrap(),
            title: "Synthetic".into(),
            input_hash: "synthetic-request".into(),
            revision: 1,
            created_at: start,
            targets: vec![ChannelTarget {
                target_id,
                input: ChannelTargetInput::Measure {
                    account_id,
                    provider: "synthetic".into(),
                    model: "synthetic".into(),
                    surface: "consumer_web".into(),
                    search_mode: "web_search".into(),
                    protocol_version: "v1".into(),
                    question_set_version: "adhoc".into(),
                    question: "Synthetic question?".into(),
                    market: "global".into(),
                    language: "en".into(),
                    scheduled_at: start,
                    sample_ordinal: 0,
                    question_binding: None,
                },
            }],
        },
    )
    .await
    .unwrap();
    let original = ChannelOutcome {
        status: ChannelOutcomeStatus::Unknown,
        detail: Some("interpretation unavailable".into()),
        occurred_at: observed_at,
        raw_answer: None,
        citations: vec![],
        public_url: None,
        screenshot_ref: None,
        connector_version: Some("synthetic-live".into()),
        fixture: false,
        runner_evidence: vec![json!({
            "kind":"observation_capture","schema_version":"geo.observation.capture.v1","phase":"source",
            "source_json":source_json,"source_sha256":source_sha256,"observed_at":observed_at,
        })],
    };
    sqlx::query(
        "INSERT INTO channel_execution_attempts \
         (attempt_id,operator_id,tenant_id,project_id,target_id,account_id,target_kind,claimed_at,outcome,received_at) \
         VALUES ($1,$2,$3,$4,$5,$6,'measure',$7,$8,$9)",
    ).bind(attempt_id).bind(operator).bind(tenant).bind(project).bind(target_id).bind(account_id)
        .bind(start).bind(serde_json::to_value(&original).unwrap()).bind(received_at)
        .execute(database.pool()).await.unwrap();
    captures
        .save(
            &scope,
            ObservationCaptureInput {
                capture_id,
                target_id,
                attempt_id,
                account_id,
                runner_session_id: Uuid::new_v4(),
                original_identity: None,
                ordinal: 0,
                observed_at,
                snapshot: ObservationCaptureSnapshot::Source {
                    source_json,
                    source_sha256: source_sha256.clone(),
                },
                owned_conversation: None,
                completion: None,
            },
        )
        .await
        .unwrap();
    let request = ObservationAnalysisRequest {
        revision_id: Uuid::new_v4(),
        target_id,
        attempt_id,
        source: ObservationAnalysisSource::Capture { capture_id },
        source_sha256,
        observed_at,
        prompt_version: "extract.v1".into(),
        parser_version: "ground.v1".into(),
    };
    let hash = sha256_hex(b"synthetic-request");
    let queued = store
        .create(&scope, "first-analysis", &hash, request.clone(), created_at)
        .await
        .unwrap();
    assert_eq!(queued.state, ObservationAnalysisState::Queued);
    let mut duplicate = request.clone();
    duplicate.revision_id = Uuid::new_v4();
    assert_eq!(
        queued,
        store
            .create(
                &scope,
                "first-analysis",
                &hash,
                duplicate.clone(),
                created_at
            )
            .await
            .unwrap()
    );
    duplicate.parser_version = "ground.v2".into();
    assert_eq!(
        store
            .create(&scope, "first-analysis", &hash, duplicate, created_at)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert!(
        store
            .get(&other, request.revision_id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .claim(&other, request.revision_id, created_at)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    assert!(
        store
            .create(&other, "first-analysis", &hash, request.clone(), created_at)
            .await
            .is_err()
    );

    let (left, right) = tokio::join!(
        store.claim(&scope, request.revision_id, created_at),
        store.claim(&scope, request.revision_id, created_at),
    );
    let claims: Vec<_> = [left.unwrap(), right.unwrap()]
        .into_iter()
        .flatten()
        .collect();
    assert_eq!(claims.len(), 1);
    let claim = &claims[0];
    let result = ObservationAnalysisResult {
        actual_model: Some("synthetic".into()),
        candidate_json: Some(r#"{"decision":"unverified"}"#.into()),
        outcome: ObservationAnalysisOutcome::Unverified {
            reason: "model_unverified".into(),
        },
        prompt_tokens: 1,
        completion_tokens: 1,
    };
    assert_eq!(
        store
            .finish(
                &scope,
                request.revision_id,
                Uuid::new_v4(),
                result.clone(),
                created_at
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        store
            .finish(
                &other,
                request.revision_id,
                claim.claim_token,
                result.clone(),
                created_at
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    let analyzed_at = created_at + Duration::seconds(1);
    let terminal = store
        .finish(
            &scope,
            request.revision_id,
            claim.claim_token,
            result.clone(),
            analyzed_at,
        )
        .await
        .unwrap();
    assert_eq!(terminal.state, ObservationAnalysisState::Completed);
    assert_eq!(terminal.request.observed_at, observed_at);
    assert_eq!(terminal.analyzed_at, Some(analyzed_at));
    assert_eq!(
        terminal,
        store
            .finish(
                &scope,
                request.revision_id,
                claim.claim_token,
                result.clone(),
                analyzed_at
            )
            .await
            .unwrap()
    );
    let mut changed = result;
    changed.completion_tokens = 2;
    assert_eq!(
        store
            .finish(
                &scope,
                request.revision_id,
                claim.claim_token,
                changed,
                analyzed_at
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert!(
        store
            .claim(&scope, request.revision_id, analyzed_at)
            .await
            .unwrap()
            .is_none()
    );
    let restarted = PgObservationAnalysisRepository::from_database(&database);
    assert_eq!(
        restarted.get(&scope, request.revision_id).await.unwrap(),
        Some(terminal)
    );
    let mut legacy = request.clone();
    legacy.revision_id = Uuid::new_v4();
    legacy.source = ObservationAnalysisSource::AttemptEvidence { evidence_index: 0 };
    restarted
        .create(&scope, "legacy-analysis", &hash, legacy.clone(), created_at)
        .await
        .unwrap();
    let page = restarted
        .list_for_attempt(&scope, target_id, attempt_id, None, 1)
        .await
        .unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(
        restarted
            .list_for_attempt(
                &scope,
                target_id,
                attempt_id,
                Some(page[0].request.revision_id),
                1
            )
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        restarted
            .list_for_attempt(
                &other,
                target_id,
                attempt_id,
                Some(page[0].request.revision_id),
                1
            )
            .await
            .is_err()
    );
    legacy.revision_id = Uuid::new_v4();
    legacy.source_sha256 = "0".repeat(64);
    assert!(
        restarted
            .create(&scope, "tampered", &hash, legacy, created_at)
            .await
            .is_err()
    );
    let saved = jobs.get_target(&scope, target_id).await.unwrap();
    assert_eq!(saved.attempts.len(), 1);
    assert_eq!(saved.attempts[0].outcome.as_ref(), Some(&original));
    let cleanup_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM provider_conversation_cleanup WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3",
    ).bind(operator).bind(tenant).bind(project).fetch_one(database.pool()).await.unwrap();
    assert_eq!(cleanup_count, 0);
}
