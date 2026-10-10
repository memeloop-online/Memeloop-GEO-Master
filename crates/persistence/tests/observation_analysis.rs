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
    let saved_sources = captures
        .list_sources_for_attempt(&scope, target_id, attempt_id)
        .await
        .unwrap();
    assert_eq!(saved_sources.len(), 1);
    assert_eq!(saved_sources[0].input.capture_id, capture_id);
    assert!(
        captures
            .list_sources_for_attempt(&other, target_id, attempt_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        captures
            .list_sources_for_attempt(&scope, target_id, Uuid::new_v4())
            .await
            .unwrap()
            .is_empty()
    );
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
        config_revision: None,
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
    let stale_running = restarted
        .claim(&scope, legacy.revision_id, created_at)
        .await
        .unwrap()
        .unwrap();
    let mut stale_queued = request.clone();
    stale_queued.revision_id = Uuid::new_v4();
    restarted
        .create(
            &scope,
            "stale-queued",
            &hash,
            stale_queued.clone(),
            created_at,
        )
        .await
        .unwrap();
    let cutoff = created_at + chrono::Duration::seconds(180);
    restarted
        .interrupt_stale(&other, target_id, attempt_id, cutoff, cutoff)
        .await
        .unwrap();
    assert_eq!(
        restarted
            .get(&scope, legacy.revision_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        ObservationAnalysisState::Running
    );
    restarted
        .interrupt_stale(&scope, target_id, attempt_id, cutoff, cutoff)
        .await
        .unwrap();
    for id in [legacy.revision_id, stale_queued.revision_id] {
        let interrupted = restarted.get(&scope, id).await.unwrap().unwrap();
        assert_eq!(interrupted.state, ObservationAnalysisState::Completed);
        assert!(matches!(interrupted.result.unwrap().outcome,
            ObservationAnalysisOutcome::Failed { code } if code == "analysis_interrupted"));
        assert!(restarted.claim(&scope, id, cutoff).await.unwrap().is_none());
    }
    assert!(
        restarted
            .finish(
                &scope,
                legacy.revision_id,
                stale_running.claim_token,
                ObservationAnalysisResult {
                    config_revision: None,
                    actual_model: None,
                    candidate_json: None,
                    outcome: ObservationAnalysisOutcome::Failed {
                        code: "late".into()
                    },
                    prompt_tokens: 0,
                    completion_tokens: 0
                },
                cutoff
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
    assert_grounded_projection(&database, &scope, &other, &request, created_at).await;
    let saved = jobs.get_target(&scope, target_id).await.unwrap();
    assert_eq!(saved.attempts.len(), 1);
    assert_eq!(saved.attempts[0].outcome.as_ref(), Some(&original));
    let cleanup_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM provider_conversation_cleanup WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3",
    ).bind(operator).bind(tenant).bind(project).fetch_one(database.pool()).await.unwrap();
    assert_eq!(cleanup_count, 0);
}

async fn assert_grounded_projection(
    database: &Database,
    scope: &TenantScope,
    other: &TenantScope,
    request: &ObservationAnalysisRequest,
    created_at: chrono::DateTime<Utc>,
) {
    let store = PgObservationAnalysisRepository::from_database(database);
    let pair = (request.target_id, request.attempt_id);
    let first_at = created_at + Duration::seconds(300);
    let tied_at = first_at + Duration::seconds(1);
    let later_at = first_at + Duration::seconds(2);
    let mut ids = [Uuid::new_v4(), Uuid::new_v4()];
    ids.sort();
    let first_id = Uuid::new_v4();
    let late_id = Uuid::new_v4();
    for (id, accepted, completed) in [
        (first_id, first_at, first_at),
        (ids[0], tied_at, tied_at),
        (ids[1], tied_at, tied_at),
        (late_id, later_at, later_at + Duration::hours(1)),
    ] {
        let mut revision_request = request.clone();
        revision_request.revision_id = id;
        store
            .create(
                scope,
                &format!("projection-{id}"),
                &sha256_hex(b"projection"),
                revision_request,
                accepted,
            )
            .await
            .unwrap();
        let claim = store.claim(scope, id, accepted).await.unwrap().unwrap();
        store.finish(scope, id, claim.claim_token, ObservationAnalysisResult {
            config_revision: None,
            actual_model: Some("synthetic".into()),
            candidate_json: Some(r#"{"decision":"answer"}"#.into()),
            outcome: ObservationAnalysisOutcome::Grounded {
                raw_answer: "Synthetic saved response".into(),
                citations: vec![],
                audit: json!({"refs":[{"pointer":"/messages/0/content","quote":"Synthetic saved response"}]}),
            },
            prompt_tokens: 1,
            completion_tokens: 1,
        }, completed).await.unwrap();
    }
    // A first-page history shortcut would miss every grounded revision here.
    sqlx::query(
        "INSERT INTO observation_analyses \
         (revision_id,operator_id,tenant_id,project_id,target_id,attempt_id,\
          idempotency_key_hash,request_digest,request,state,claim_token,created_at,started_at,analyzed_at,result) \
         SELECT id,$1,$2,$3,$4,$5,md5(id::text)||md5(id::text),$6,\
         jsonb_set($7,'{revision_id}',to_jsonb(id::text)),'completed',id,$8,$8,$8,$9 \
         FROM unnest($10::uuid[]) ids(id)",
    )
    .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
    .bind(scope.project_id.unwrap().as_uuid()).bind(pair.0).bind(pair.1)
    .bind(sha256_hex(b"failed-history")).bind(serde_json::to_value(request).unwrap())
    .bind(later_at).bind(json!({
        "actual_model":null,"candidate_json":null,
        "outcome":{"status":"failed","code":"analysis_interrupted"},
        "prompt_tokens":0,"completion_tokens":0
    }))
    .bind((0..2048).map(|_| Uuid::new_v4()).collect::<Vec<_>>())
    .execute(database.pool()).await.unwrap();

    for (cutoff, expected) in [
        (first_at - Duration::microseconds(1), None),
        (first_at, Some(first_id)),
        (tied_at, Some(ids[1])),
        (later_at, Some(ids[1])),
        (later_at + Duration::hours(1), Some(late_id)),
    ] {
        let selected = store
            .latest_grounded_for_attempts(
                scope,
                &[
                    pair,
                    pair,
                    (pair.0, Uuid::new_v4()),
                    (Uuid::new_v4(), pair.1),
                ],
                cutoff,
            )
            .await
            .unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|r| r.request.revision_id)
                .collect::<Vec<_>>(),
            expected.into_iter().collect::<Vec<_>>()
        );
    }
    for foreign in [
        other.clone(),
        TenantScope::new(scope.operator_id, Uuid::new_v4().into(), scope.project_id),
        TenantScope::new(Uuid::new_v4().into(), scope.tenant_id, scope.project_id),
    ] {
        assert!(
            store
                .latest_grounded_for_attempts(&foreign, &[pair], later_at)
                .await
                .unwrap()
                .is_empty()
        );
    }
    assert!(
        store
            .latest_grounded_for_attempts(scope, &[], later_at)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .latest_grounded_for_attempts(scope, &vec![pair; 1000], later_at)
            .await
            .is_ok()
    );
    assert_eq!(
        store
            .latest_grounded_for_attempts(scope, &vec![pair; 1001], later_at)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );
    assert!(
        store
            .latest_grounded_for_attempts(scope, &[(Uuid::nil(), pair.1)], later_at)
            .await
            .is_err()
    );
    let no_project = TenantScope::new(scope.operator_id, scope.tenant_id, None);
    assert_eq!(
        store
            .latest_grounded_for_attempts(&no_project, &[], later_at)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Forbidden
    );

    // Verify the ordered partial index supports the inner seek even when this
    // disposable database is too small for the planner to prefer an index.
    let mut tx = database.pool().begin().await.unwrap();
    sqlx::query("SET LOCAL enable_seqscan=off")
        .execute(&mut *tx)
        .await
        .unwrap();
    let plan: Vec<String> = sqlx::query_scalar(
        "EXPLAIN SELECT * FROM observation_analyses a \
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND target_id=$4 AND attempt_id=$5 \
         AND state='completed' AND result->'outcome'->>'status'='grounded' \
         AND created_at<=$6 AND analyzed_at<=$6 AND (request->>'observed_at')::timestamptz<=$6 \
         ORDER BY created_at DESC,revision_id DESC LIMIT 1",
    ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
        .bind(scope.project_id.unwrap().as_uuid()).bind(pair.0).bind(pair.1).bind(later_at)
        .fetch_all(&mut *tx).await.unwrap();
    assert!(
        plan.iter()
            .any(|line| line.contains("observation_analyses_grounded_projection"))
    );
    assert!(!plan.iter().any(|line| line.contains("Sort")));
    tx.rollback().await.unwrap();
}
