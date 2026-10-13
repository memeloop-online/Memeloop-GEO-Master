use chrono::Utc;
use geo_domain::{
    CapturedConversation, CapturedConversationPurpose, ChannelJobRepository, ChannelTarget,
    ChannelTargetInput, ConversationCorrelation, ErrorCode, ObservationCaptureInput,
    ObservationCaptureRepository, ObservationCaptureSnapshot, ObservationCompletionRecoveryInput,
    ObservationCompletionRecoveryJobOutcome, ObservationCompletionRecoveryJobRepository,
    ObservationCompletionRecoveryRepository, ObservationProviderIdentity,
    ProviderConversationCleanupRepository, StandaloneMeasurementPlan, TenantScope, sha256_hex,
};
use geo_persistence::{
    Database, DatabaseConfig, PgChannelJobRepository, PgObservationCaptureRepository,
    PgObservationCompletionRecoveryJobRepository, PgObservationCompletionRecoveryRepository,
    PgProviderConversationCleanupRepository,
};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn immutable_scoped_recovery_replay_restart_and_revalidation() {
    let config = DatabaseConfig::from_url(
        std::env::var("GEO_TEST_DATABASE_URL").expect("disposable PostgreSQL URL required"),
    )
    .unwrap();
    let database = Database::connect_and_migrate(&config).await.unwrap();
    let (operator, tenant, project, other_project, account, target, attempt) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'Synthetic')")
        .bind(operator)
        .bind(format!("recovery-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,'Synthetic')")
        .bind(tenant).bind(operator).bind(format!("recovery-{tenant}"))
        .execute(database.pool()).await.unwrap();
    for id in [project, other_project] {
        sqlx::query("INSERT INTO projects (project_id,operator_id,tenant_id,slug,display_name) VALUES ($1,$2,$3,$4,'Synthetic')")
            .bind(id).bind(operator).bind(tenant).bind(format!("recovery-{id}"))
            .execute(database.pool()).await.unwrap();
    }
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.into()));
    let plan = Uuid::new_v4();
    PgChannelJobRepository::from_database(&database)
        .create_measurement_plan(
            &scope,
            &format!("recovery-{plan}"),
            "synthetic",
            StandaloneMeasurementPlan {
                plan_id: plan,
                project_id: scope.project_id.unwrap(),
                title: "Synthetic recovery".into(),
                input_hash: "synthetic".into(),
                revision: 1,
                created_at: Utc::now(),
                targets: vec![ChannelTarget {
                    target_id: target,
                    input: ChannelTargetInput::Measure {
                        account_id: account,
                        provider: "deepseek".into(),
                        model: "synthetic-model".into(),
                        surface: "consumer_web".into(),
                        search_mode: "web_search".into(),
                        protocol_version: "v1".into(),
                        question_set_version: "adhoc".into(),
                        question: "Synthetic question?".into(),
                        market: "global".into(),
                        language: "en".into(),
                        scheduled_at: Utc::now(),
                        sample_ordinal: 0,
                        question_binding: None,
                    },
                }],
            },
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO channel_execution_attempts \
         (attempt_id,operator_id,tenant_id,project_id,target_id,account_id,target_kind,claimed_at) \
         VALUES ($1,$2,$3,$4,$5,$6,'measure',$7)",
    )
    .bind(attempt)
    .bind(operator)
    .bind(tenant)
    .bind(project)
    .bind(target)
    .bind(account)
    .bind(Utc::now())
    .execute(database.pool())
    .await
    .unwrap();
    let observed_at = "2026-01-01T00:00:00Z".parse().unwrap();
    let source_json = json!({
        "messages": [
            {"event": "ready", "data": {"request_message_id": 1, "response_message_id": 2, "model_type": "synthetic-model"}},
            {"event": "delta", "data": {"v": "synthetic answer"}},
            {"event": "finish", "raw_data": ""}
        ],
        "ownership": {
            "schema_version": "geo.deepseek.ownership.v1", "provider": "deepseek",
            "creation": {"source": "browser_observed_create", "chat_session_id": "synthetic-chat"},
            "submission": {
                "chat_session_id": "synthetic-chat", "parent_message_id": null,
                "model_type": "synthetic-model", "prompt_sha256": "0".repeat(64),
                "search_enabled": true, "thinking_enabled": false
            }
        }
    }).to_string();
    let history_json = json!({
        "source": "browser_observed_history", "observed_at": "2026-01-01T00:00:00Z",
        "cache_request": {"cache_version": null, "cache_reset_at": null},
        "cache_control": "REPLACE", "chat_session_id": "synthetic-chat",
        "messages": [
            {"message_id": 1, "parent_id": null, "role": "USER", "status": "FINISHED", "has_pending_fragment": false},
            {"message_id": 2, "parent_id": 1, "role": "ASSISTANT", "status": "FINISHED", "has_pending_fragment": false}
        ], "current_message_id": 2
    }).to_string();
    let identity = ObservationProviderIdentity {
        provider: "deepseek".into(),
        platform_account_id: "synthetic-account".into(),
    };
    let original = ObservationCaptureInput {
        capture_id: Uuid::new_v4(),
        target_id: target,
        attempt_id: attempt,
        account_id: account,
        runner_session_id: Uuid::new_v4(),
        original_identity: Some(identity.clone()),
        ordinal: 0,
        observed_at,
        snapshot: ObservationCaptureSnapshot::Source {
            source_sha256: sha256_hex(source_json.as_bytes()),
            source_json: source_json.clone(),
        },
        owned_conversation: Some(CapturedConversation {
            provider: "deepseek".into(),
            external_conversation_id: "synthetic-chat".into(),
            purpose: CapturedConversationPurpose::Measurement,
            correlation: ConversationCorrelation::CreateResponse,
        }),
        completion: None,
    };
    let captures = PgObservationCaptureRepository::from_database(&database);
    let original_receipt = captures.save(&scope, original.clone()).await.unwrap();
    let jobs = PgObservationCompletionRecoveryJobRepository::from_database(&database);
    assert!(jobs.scan_unqueued(Utc::now(), None, 0).await.is_err());
    assert!(jobs.scan_due(Utc::now(), None, 101).await.is_err());
    let as_of = Utc::now();
    let mut cursor = None;
    let mut found = false;
    loop {
        let page = jobs.scan_unqueued(as_of, cursor, 2).await.unwrap();
        if page.is_empty() {
            break;
        }
        for item in &page {
            assert!(cursor.is_none_or(|cursor| item.capture_id > cursor));
            if item.capture_id == original.capture_id {
                assert_eq!(item.scope, scope);
                found = true;
            }
        }
        cursor = page.last().map(|item| item.capture_id);
    }
    assert!(found);
    let (first, second) = tokio::join!(
        jobs.enqueue(&scope, original.capture_id),
        jobs.enqueue(&scope, original.capture_id)
    );
    let job_id = first.unwrap();
    assert_eq!(job_id, second.unwrap());
    let due_as_of = Utc::now();
    let mut due_cursor = None;
    let mut due_found = false;
    loop {
        let page = jobs.scan_due(due_as_of, due_cursor, 2).await.unwrap();
        if page.is_empty() {
            break;
        }
        due_found |= page
            .iter()
            .any(|item| item.job_id == job_id && item.scope == scope);
        assert!(
            page.iter()
                .all(|item| due_cursor.is_none_or(|id| item.job_id > id))
        );
        due_cursor = page.last().map(|item| item.job_id);
    }
    assert!(due_found);
    let wrong_scope = TenantScope::new(operator.into(), tenant.into(), Some(other_project.into()));
    assert!(
        jobs.enqueue(&wrong_scope, original.capture_id)
            .await
            .is_err()
    );
    assert!(jobs.claim(&wrong_scope, job_id).await.unwrap().is_none());
    let (first, second) = tokio::join!(jobs.claim(&scope, job_id), jobs.claim(&scope, job_id));
    let first = first.unwrap();
    let second = second.unwrap();
    assert_ne!(first.is_some(), second.is_some());
    let recovery_claim = first.or(second).unwrap();
    assert_eq!(recovery_claim.original.input, original);
    assert_eq!(recovery_claim.request_binding.request_message_id, "1");
    assert_eq!(recovery_claim.request_binding.response_message_id, "2");
    assert!(
        jobs.finish(
            &scope,
            job_id,
            recovery_claim.lease_id,
            ObservationCompletionRecoveryJobOutcome::Recovered
        )
        .await
        .is_err()
    );
    assert!(
        jobs.finish(
            &wrong_scope,
            job_id,
            recovery_claim.lease_id,
            ObservationCompletionRecoveryJobOutcome::Retry
        )
        .await
        .is_err()
    );
    jobs.finish(
        &scope,
        job_id,
        recovery_claim.lease_id,
        ObservationCompletionRecoveryJobOutcome::Retry,
    )
    .await
    .unwrap();
    assert!(jobs.claim(&scope, job_id).await.unwrap().is_none());
    sqlx::query(
        "UPDATE observation_completion_recovery_jobs SET next_attempt_at=now() WHERE job_id=$1",
    )
    .bind(job_id)
    .execute(database.pool())
    .await
    .unwrap();
    let restarted_jobs = PgObservationCompletionRecoveryJobRepository::from_database(&database);
    let renewed = restarted_jobs.claim(&scope, job_id).await.unwrap().unwrap();
    assert_eq!(renewed.original, recovery_claim.original);
    assert_eq!(renewed.request_binding, recovery_claim.request_binding);
    assert_ne!(renewed.lease_id, recovery_claim.lease_id);
    assert!(
        jobs.finish(
            &scope,
            job_id,
            recovery_claim.lease_id,
            ObservationCompletionRecoveryJobOutcome::Retained
        )
        .await
        .is_err()
    );
    jobs.finish(
        &scope,
        job_id,
        renewed.lease_id,
        ObservationCompletionRecoveryJobOutcome::NeedsLogin,
    )
    .await
    .unwrap();
    let login_delay: f64 = sqlx::query_scalar(
        "SELECT EXTRACT(EPOCH FROM next_attempt_at-updated_at)::double precision \
         FROM observation_completion_recovery_jobs WHERE job_id=$1",
    )
    .bind(job_id)
    .fetch_one(database.pool())
    .await
    .unwrap();
    assert!((899.0..=3601.0).contains(&login_delay));
    sqlx::query(
        "UPDATE observation_completion_recovery_jobs SET next_attempt_at=now() WHERE job_id=$1",
    )
    .bind(job_id)
    .execute(database.pool())
    .await
    .unwrap();
    let renewed = restarted_jobs.claim(&scope, job_id).await.unwrap().unwrap();
    // Expiration only permits another read-only claim; no browser/account state
    // is touched. Late outcomes from the expired execution are fenced.
    sqlx::query("UPDATE observation_completion_recovery_jobs SET lease_until=now()-interval '1 second' WHERE job_id=$1")
        .bind(job_id).execute(database.pool()).await.unwrap();
    let recovery_claim = restarted_jobs.claim(&scope, job_id).await.unwrap().unwrap();
    assert!(
        jobs.finish(
            &scope,
            job_id,
            renewed.lease_id,
            ObservationCompletionRecoveryJobOutcome::Retry
        )
        .await
        .is_err()
    );
    let audit_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM observation_completion_recovery_job_attempts WHERE job_id=$1",
    )
    .bind(job_id)
    .fetch_one(database.pool())
    .await
    .unwrap();
    assert_eq!(audit_count, 7);
    assert!(sqlx::query("UPDATE observation_completion_recovery_job_attempts SET event='retained' WHERE job_id=$1")
        .bind(job_id).execute(database.pool()).await.is_err());
    let input = ObservationCompletionRecoveryInput {
        recovery_id: Uuid::new_v4(),
        scope: scope.clone(),
        source_capture_id: original.capture_id,
        source_sha256: sha256_hex(source_json.as_bytes()),
        source_receipt_sha256: original_receipt.digest_sha256.clone(),
        recovery_execution_id: Uuid::new_v4(),
        runner_session_id: Uuid::new_v4(),
        observed_at,
        recovery_identity: identity,
        history_sha256: sha256_hex(history_json.as_bytes()),
        history_json,
    };
    let store = PgObservationCompletionRecoveryRepository::from_database(&database);
    assert!(
        store
            .get_by_source(&scope, original.capture_id)
            .await
            .unwrap()
            .is_none()
    );
    let cleanup = PgProviderConversationCleanupRepository::from_database(&database);
    let cleanup_id = cleanup.enqueue(&scope, original.capture_id).await.unwrap();
    // A sidecar cannot bypass a still-running original operation.
    assert!(cleanup.claim(&scope, cleanup_id).await.unwrap().is_none());
    sqlx::query("UPDATE channel_execution_attempts SET outcome='{\"status\":\"unknown\"}',received_at=now() WHERE attempt_id=$1")
        .bind(attempt).execute(database.pool()).await.unwrap();
    let claim = cleanup.claim(&scope, cleanup_id).await.unwrap().unwrap();
    assert!(claim.retained_message_inventory_sha256.is_none());
    let reservation_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO channel_account_preflight_reservations \
         (operator_id,account_id,reservation_id,expires_at) VALUES ($1,$2,$3,now()+interval '2 minutes')",
    ).bind(operator).bind(account).bind(reservation_id)
        .execute(database.pool()).await.unwrap();
    assert!(
        cleanup
            .authorize_delete(&scope, cleanup_id, claim.lease_id, reservation_id)
            .await
            .is_err()
    );
    let receipt = store.save(&scope, input.clone()).await.unwrap();
    jobs.finish(
        &scope,
        job_id,
        recovery_claim.lease_id,
        ObservationCompletionRecoveryJobOutcome::Recovered,
    )
    .await
    .unwrap();
    assert!(jobs.claim(&scope, job_id).await.unwrap().is_none());
    assert_eq!(
        jobs.enqueue(&scope, original.capture_id).await.unwrap(),
        job_id
    );
    assert_eq!(
        captures
            .get(&scope, original.capture_id)
            .await
            .unwrap()
            .unwrap()
            .input,
        original
    );
    assert_eq!(
        store
            .get_by_source(&scope, original.capture_id)
            .await
            .unwrap()
            .unwrap()
            .receipt,
        receipt
    );
    assert!(
        store
            .get_by_source(&scope, Uuid::new_v4())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        cleanup
            .authorize_delete(&scope, cleanup_id, claim.lease_id, Uuid::new_v4())
            .await
            .is_err()
    );
    let authorized = cleanup
        .authorize_delete(&scope, cleanup_id, claim.lease_id, reservation_id)
        .await
        .unwrap();
    assert_eq!(authorized.capture_id, original.capture_id);
    assert_eq!(
        authorized.original_runner_session_id,
        Some(original.runner_session_id)
    );
    assert_eq!(
        authorized.retained_message_inventory_sha256,
        Some(sha256_hex(br#"[["1","user"],["2","assistant"]]"#))
    );
    cleanup
        .finish(
            &scope,
            cleanup_id,
            claim.lease_id,
            geo_domain::ProviderCleanupOutcome::Failed,
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE provider_conversation_cleanup SET next_attempt_at=now() WHERE cleanup_id=$1",
    )
    .bind(cleanup_id)
    .execute(database.pool())
    .await
    .unwrap();
    let claim = cleanup.claim(&scope, cleanup_id).await.unwrap().unwrap();
    assert_eq!(
        claim.retained_message_inventory_sha256,
        authorized.retained_message_inventory_sha256
    );
    assert_eq!(claim.capture_id, original.capture_id);
    assert!(!claim.has_prior_delete_attempt);
    assert_eq!(store.save(&scope, input.clone()).await.unwrap(), receipt);
    let other_scope = TenantScope::new(operator.into(), tenant.into(), Some(other_project.into()));
    let no_project = TenantScope::new(operator.into(), tenant.into(), None);
    for wrong_scope in [
        other_scope.clone(),
        TenantScope::new(Uuid::new_v4().into(), tenant.into(), Some(project.into())),
        TenantScope::new(operator.into(), Uuid::new_v4().into(), Some(project.into())),
    ] {
        assert!(
            store
                .get_by_source(&wrong_scope, original.capture_id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .get(&wrong_scope, input.recovery_id)
                .await
                .unwrap()
                .is_none()
        );
        let mut rebound = input.clone();
        rebound.scope = wrong_scope.clone();
        assert_eq!(
            store.save(&wrong_scope, rebound).await.unwrap_err().code,
            ErrorCode::Forbidden
        );
    }
    assert_eq!(
        store
            .get(&no_project, input.recovery_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Forbidden
    );
    for field in 0..3 {
        let mut mismatch = input.clone();
        mismatch.recovery_id = Uuid::new_v4();
        mismatch.recovery_execution_id = Uuid::new_v4();
        match field {
            0 => mismatch.source_sha256 = "0".repeat(64),
            1 => mismatch.source_receipt_sha256 = "0".repeat(64),
            _ => mismatch.source_capture_id = Uuid::new_v4(),
        }
        assert!(store.save(&scope, mismatch).await.is_err());
    }
    let mut concurrent = input.clone();
    concurrent.recovery_id = Uuid::new_v4();
    concurrent.recovery_execution_id = Uuid::new_v4();
    let (first, second) = tokio::join!(
        store.save(&scope, concurrent.clone()),
        store.save(&scope, concurrent)
    );
    assert_eq!(first.unwrap(), second.unwrap());
    let mut conflicting = input.clone();
    conflicting.runner_session_id = Uuid::new_v4();
    assert_eq!(
        store.save(&scope, conflicting).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    let mut duplicate_execution = input.clone();
    duplicate_execution.recovery_id = Uuid::new_v4();
    assert_eq!(
        store
            .save(&scope, duplicate_execution)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    for statement in [
        "UPDATE observation_completion_recoveries SET input_hash=input_hash WHERE recovery_id=$1",
        "DELETE FROM observation_completion_recoveries WHERE recovery_id=$1",
    ] {
        assert!(
            sqlx::query(statement)
                .bind(input.recovery_id)
                .execute(database.pool())
                .await
                .is_err()
        );
    }
    // Direct SQL cannot bypass the scoped foreign key, size, or hash constraints.
    for mutation in 0..3 {
        let mut invalid = input.clone();
        invalid.recovery_id = Uuid::new_v4();
        invalid.recovery_execution_id = Uuid::new_v4();
        if mutation == 1 {
            invalid.history_json = "x".repeat(750_001);
        }
        let inserted = sqlx::query(
            "INSERT INTO observation_completion_recoveries \
             (recovery_id,operator_id,tenant_id,project_id,source_capture_id,recovery_execution_id,input_hash,input) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
        ).bind(invalid.recovery_id).bind(operator).bind(tenant)
            .bind(if mutation == 0 { other_project } else { project })
            .bind(original.capture_id).bind(invalid.recovery_execution_id)
            .bind(if mutation == 2 { "invalid".to_owned() } else { "0".repeat(64) })
            .bind(serde_json::to_value(invalid).unwrap()).execute(database.pool()).await;
        assert!(inserted.is_err());
    }
    // An untrusted database projection with a syntactically valid but wrong hash
    // must not become validated evidence on read.
    let mut corrupt = input.clone();
    corrupt.recovery_id = Uuid::new_v4();
    corrupt.recovery_execution_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO observation_completion_recoveries \
         (recovery_id,operator_id,tenant_id,project_id,source_capture_id,recovery_execution_id,input_hash,input) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    ).bind(corrupt.recovery_id).bind(operator).bind(tenant).bind(project)
        .bind(original.capture_id).bind(corrupt.recovery_execution_id).bind("0".repeat(64))
        .bind(serde_json::to_value(&corrupt).unwrap()).execute(database.pool()).await.unwrap();
    assert_eq!(
        store
            .get(&scope, corrupt.recovery_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Internal
    );
    assert!(
        store
            .get_by_source(&scope, original.capture_id)
            .await
            .is_err()
    );
    assert!(
        cleanup
            .authorize_delete(&scope, cleanup_id, claim.lease_id, reservation_id)
            .await
            .is_err()
    );
    drop(store);
    let restarted_database = Database::connect_and_migrate(&config).await.unwrap();
    let restarted = PgObservationCompletionRecoveryRepository::from_database(&restarted_database);
    let recovered = restarted
        .get(&scope, input.recovery_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.input, input);
    assert_eq!(recovered.receipt, receipt);
    let unchanged = captures
        .get(&scope, original.capture_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.input, original);
    assert_eq!(unchanged.receipt, original_receipt);
    assert!(!unchanged.has_complete_conversation_evidence(&scope));
    // Missing immutable prerequisites are remembered once as retained and
    // cannot enter a restore/retry loop, even after repository restart.
    for missing_identity in [false, true] {
        let mut ineligible = original.clone();
        ineligible.capture_id = Uuid::new_v4();
        ineligible.runner_session_id = Uuid::new_v4();
        if missing_identity {
            ineligible.original_identity = None;
        } else {
            let mut source: serde_json::Value = serde_json::from_str(&source_json).unwrap();
            source["messages"].as_array_mut().unwrap().remove(0);
            let source_json = source.to_string();
            ineligible.snapshot = ObservationCaptureSnapshot::Source {
                source_sha256: sha256_hex(source_json.as_bytes()),
                source_json,
            };
        }
        captures.save(&scope, ineligible.clone()).await.unwrap();
        let retained = jobs.enqueue(&scope, ineligible.capture_id).await.unwrap();
        assert_eq!(
            jobs.enqueue(&scope, ineligible.capture_id).await.unwrap(),
            retained
        );
        assert!(
            restarted_jobs
                .claim(&scope, retained)
                .await
                .unwrap()
                .is_none()
        );
        let state: String = sqlx::query_scalar(
            "SELECT state FROM observation_completion_recovery_jobs WHERE job_id=$1",
        )
        .bind(retained)
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(state, "retained");
        assert_eq!(
            captures
                .get(&scope, ineligible.capture_id)
                .await
                .unwrap()
                .unwrap()
                .input,
            ineligible
        );
    }
}
