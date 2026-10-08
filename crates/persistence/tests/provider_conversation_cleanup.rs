use chrono::Utc;
use geo_domain::{
    CapturedConversation, CapturedConversationPurpose, ChannelTargetInput, ConversationCorrelation,
    ExtractionRoute, ObservationCaptureInput, ObservationCaptureRepository,
    ObservationCaptureSnapshot, ProviderCleanupAction, ProviderCleanupOutcome,
    ProviderConversationCleanupRepository, TenantScope, sha256_hex,
};
use geo_persistence::{
    Database, DatabaseConfig, PgObservationCaptureRepository,
    PgProviderConversationCleanupRepository,
};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn persisted_ownership_fenced_claim_unknown_recovery_and_capture_retention() {
    let config = DatabaseConfig::from_url(std::env::var("GEO_TEST_DATABASE_URL").unwrap()).unwrap();
    let database = Database::connect_and_migrate(&config).await.unwrap();
    let (operator, tenant, project, account, target, attempt, plan) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    sqlx::query("INSERT INTO operators(operator_id,slug,display_name) VALUES($1,$2,'Synthetic')")
        .bind(operator)
        .bind(format!("cleanup-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants(tenant_id,operator_id,slug,display_name) VALUES($1,$2,$3,'Synthetic')",
    )
    .bind(tenant)
    .bind(operator)
    .bind(format!("cleanup-{tenant}"))
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO projects(project_id,operator_id,tenant_id,slug,display_name) VALUES($1,$2,$3,$4,'Synthetic')")
        .bind(project).bind(operator).bind(tenant).bind(format!("cleanup-{project}")).execute(database.pool()).await.unwrap();
    sqlx::query("INSERT INTO measurement_execution_plans(plan_id,operator_id,tenant_id,project_id,idempotency_key,request_hash,input_hash,revision,plan,created_at) VALUES($1,$2,$3,$4,$5,'request','frozen',1,'{}',now())")
        .bind(plan).bind(operator).bind(tenant).bind(project).bind(format!("cleanup-{plan}")).execute(database.pool()).await.unwrap();
    let frozen = ChannelTargetInput::Measure {
        account_id: account,
        provider: "kimi".into(),
        model: "fixed".into(),
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
    };
    sqlx::query("INSERT INTO channel_execution_targets(target_id,operator_id,tenant_id,project_id,kind,frozen_input,ordinal,measurement_plan_id) VALUES($1,$2,$3,$4,'measure',$5,0,$6)")
        .bind(target).bind(operator).bind(tenant).bind(project).bind(serde_json::to_value(frozen).unwrap()).bind(plan).execute(database.pool()).await.unwrap();
    sqlx::query("INSERT INTO channel_execution_attempts(attempt_id,operator_id,tenant_id,project_id,target_id,account_id,target_kind,claimed_at) VALUES($1,$2,$3,$4,$5,$6,'measure',now())")
        .bind(attempt).bind(operator).bind(tenant).bind(project).bind(target).bind(account).execute(database.pool()).await.unwrap();
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.into()));
    let captures = PgObservationCaptureRepository::from_database(&database);
    let cleanup = PgProviderConversationCleanupRepository::from_database(&database);
    let source_json = r#"{"messages":[{"text":"synthetic answer"}]}"#.to_owned();
    let input = ObservationCaptureInput {
        capture_id: Uuid::new_v4(),
        target_id: target,
        attempt_id: attempt,
        account_id: account,
        runner_session_id: Uuid::new_v4(),
        original_identity: Some(geo_domain::ObservationProviderIdentity {
            provider: "kimi".into(),
            platform_account_id: "synthetic-account".into(),
        }),
        ordinal: 0,
        observed_at: Utc::now(),
        snapshot: ObservationCaptureSnapshot::Source {
            source_sha256: sha256_hex(source_json.as_bytes()),
            source_json,
        },
        owned_conversation: Some(CapturedConversation {
            provider: "kimi".into(),
            external_conversation_id: "synthetic-chat".into(),
            purpose: CapturedConversationPurpose::Measurement,
            correlation: ConversationCorrelation::CreateResponse,
        }),
        completion: None,
    };
    assert!(cleanup.enqueue(&scope, input.capture_id).await.is_err());
    captures.save(&scope, input.clone()).await.unwrap();
    let discovered = backfill_ids(&cleanup, &scope, Utc::now()).await;
    assert_eq!(discovered, vec![input.capture_id]);
    assert!(
        backfill_ids(
            &cleanup,
            &scope,
            input.observed_at - chrono::Duration::days(1)
        )
        .await
        .is_empty()
    );
    for limit in [0, 101, usize::MAX] {
        assert!(
            cleanup
                .scan_unqueued(Utc::now(), None, limit)
                .await
                .is_err()
        );
        assert!(cleanup.scan_due(Utc::now(), None, limit).await.is_err());
    }
    let id = cleanup.enqueue(&scope, input.capture_id).await.unwrap();
    assert_eq!(cleanup.enqueue(&scope, input.capture_id).await.unwrap(), id);
    assert!(cleanup.claim_due(&scope).await.unwrap().is_none());
    assert!(cleanup.claim(&scope, id).await.unwrap().is_none());
    let legacy = ObservationCaptureInput {
        capture_id: Uuid::new_v4(),
        runner_session_id: Uuid::new_v4(),
        original_identity: None,
        ..input.clone()
    };
    captures.save(&scope, legacy.clone()).await.unwrap();
    assert!(cleanup.enqueue(&scope, legacy.capture_id).await.is_err());
    let unowned = ObservationCaptureInput {
        capture_id: Uuid::new_v4(),
        runner_session_id: Uuid::new_v4(),
        owned_conversation: None,
        ..input.clone()
    };
    captures.save(&scope, unowned.clone()).await.unwrap();
    assert!(cleanup.enqueue(&scope, unowned.capture_id).await.is_err());
    let duplicate = ObservationCaptureInput {
        capture_id: Uuid::new_v4(),
        runner_session_id: Uuid::new_v4(),
        ..input.clone()
    };
    captures.save(&scope, duplicate.clone()).await.unwrap();
    assert_eq!(
        cleanup.enqueue(&scope, duplicate.capture_id).await.unwrap(),
        id
    );
    let other_scope = TenantScope::new(operator.into(), tenant.into(), Some(Uuid::new_v4().into()));
    assert!(
        cleanup
            .enqueue(&other_scope, input.capture_id)
            .await
            .is_err()
    );
    assert!(cleanup.claim_due(&other_scope).await.unwrap().is_none());
    assert!(cleanup.claim(&other_scope, id).await.unwrap().is_none());
    let candidate_json = r#"{"decision":"unverified"}"#.to_owned();
    let candidate = ObservationCaptureInput {
        capture_id: Uuid::new_v4(),
        ordinal: 1,
        snapshot: ObservationCaptureSnapshot::Candidate {
            source_capture_id: input.capture_id,
            route: ExtractionRoute::ConfiguredModelApi,
            candidate_sha256: sha256_hex(candidate_json.as_bytes()),
            candidate_json,
            grounding_reason: Some("model_unverified".into()),
        },
        owned_conversation: Some(CapturedConversation {
            provider: "kimi".into(),
            external_conversation_id: "synthetic-extraction".into(),
            purpose: CapturedConversationPurpose::Extraction,
            correlation: ConversationCorrelation::CreateResponse,
        }),
        ..input.clone()
    };
    captures.save(&scope, candidate.clone()).await.unwrap();
    assert!(cleanup.enqueue(&scope, candidate.capture_id).await.is_err());
    // Candidate, legacy identity, unowned, and already registered duplicate
    // captures must not keep returning in the backfill.
    assert!(backfill_ids(&cleanup, &scope, Utc::now()).await.is_empty());
    // Unknown interpretation still preserves raw evidence and ends active use.
    sqlx::query("UPDATE channel_execution_attempts SET outcome='{\"status\":\"unknown\"}',received_at=now() WHERE attempt_id=$1")
        .bind(attempt).execute(database.pool()).await.unwrap();
    assert_eq!(due_ids(&cleanup, &scope, Utc::now()).await, vec![id]);
    let claim = cleanup.claim(&scope, id).await.unwrap().unwrap();
    assert_eq!(claim.action, ProviderCleanupAction::Delete);
    assert_eq!(
        Some(&claim.original_identity),
        input.original_identity.as_ref()
    );
    assert!(cleanup.claim_due(&scope).await.unwrap().is_none());
    assert!(due_ids(&cleanup, &scope, Utc::now()).await.is_empty());
    assert!(
        cleanup
            .finish(&scope, id, Uuid::new_v4(), ProviderCleanupOutcome::Deleted)
            .await
            .is_err()
    );
    sqlx::query("UPDATE provider_conversation_cleanup SET lease_until=now()-interval '1 second' WHERE cleanup_id=$1")
        .bind(id).execute(database.pool()).await.unwrap();
    let resumed = cleanup.claim_due(&scope).await.unwrap().unwrap();
    assert_eq!(resumed.action, ProviderCleanupAction::Reconcile);
    assert_ne!(claim.lease_id, resumed.lease_id);
    assert!(
        cleanup
            .finish(&scope, id, claim.lease_id, ProviderCleanupOutcome::Deleted)
            .await
            .is_err()
    );
    cleanup
        .finish(
            &scope,
            id,
            resumed.lease_id,
            ProviderCleanupOutcome::Unknown,
        )
        .await
        .unwrap();
    assert!(due_ids(&cleanup, &scope, Utc::now()).await.is_empty());
    sqlx::query(
        "UPDATE provider_conversation_cleanup SET next_attempt_at=now(),attempt_count=25 WHERE cleanup_id=$1",
    )
    .bind(id)
    .execute(database.pool())
    .await
    .unwrap();
    let reconcile = cleanup.claim_due(&scope).await.unwrap().unwrap();
    assert_eq!(reconcile.action, ProviderCleanupAction::Reconcile);
    cleanup
        .finish(
            &scope,
            id,
            reconcile.lease_id,
            ProviderCleanupOutcome::Deleted,
        )
        .await
        .unwrap();
    assert!(cleanup.claim_due(&scope).await.unwrap().is_none());
    assert!(due_ids(&cleanup, &scope, Utc::now()).await.is_empty());
    assert_eq!(
        captures
            .get(&scope, input.capture_id)
            .await
            .unwrap()
            .unwrap()
            .input,
        input
    );
    assert_eq!(
        cleanup.enqueue(&scope, duplicate.capture_id).await.unwrap(),
        id
    );
    // Raw extraction ownership survives malformed/missing interpretation and
    // is independently eligible for cleanup after the attempt has ended.
    let source_json = r#"{"messages":[{"text":"not JSON"}]}"#.to_owned();
    let raw_extraction = ObservationCaptureInput {
        capture_id: Uuid::new_v4(),
        ordinal: 2,
        snapshot: ObservationCaptureSnapshot::Extraction {
            source_capture_id: input.capture_id,
            source_sha256: sha256_hex(source_json.as_bytes()),
            source_json,
        },
        owned_conversation: candidate.owned_conversation.clone(),
        ..input.clone()
    };
    captures.save(&scope, raw_extraction.clone()).await.unwrap();
    assert_eq!(
        backfill_ids(&cleanup, &scope, Utc::now()).await,
        vec![raw_extraction.capture_id]
    );
    let raw_cleanup_id = cleanup
        .enqueue(&scope, raw_extraction.capture_id)
        .await
        .unwrap();
    assert_ne!(raw_cleanup_id, id);
    assert_eq!(
        cleanup
            .enqueue(&scope, raw_extraction.capture_id)
            .await
            .unwrap(),
        raw_cleanup_id
    );
    let raw_claim = cleanup.claim_due(&scope).await.unwrap().unwrap();
    assert_eq!(raw_claim.action, ProviderCleanupAction::Delete);
    cleanup
        .finish(
            &scope,
            raw_cleanup_id,
            raw_claim.lease_id,
            ProviderCleanupOutcome::Deleted,
        )
        .await
        .unwrap();
    assert!(cleanup.claim_due(&scope).await.unwrap().is_none());

    // Three separately owned resources exercise exclusive UUID keysets. The
    // snapshot cutoff must not admit captures saved after a scan began.
    let cutoff = Utc::now() - chrono::Duration::seconds(1);
    let mut expected_captures = Vec::new();
    let mut expected_jobs = Vec::new();
    for index in 0..3 {
        let mut next = input.clone();
        next.capture_id = Uuid::new_v4();
        next.runner_session_id = Uuid::new_v4();
        next.owned_conversation
            .as_mut()
            .unwrap()
            .external_conversation_id = format!("synthetic-page-{index}");
        captures.save(&scope, next.clone()).await.unwrap();
        expected_captures.push(next.capture_id);
    }
    assert!(backfill_ids(&cleanup, &scope, cutoff).await.is_empty());
    expected_captures.sort();
    assert_eq!(
        backfill_ids(&cleanup, &scope, Utc::now()).await,
        expected_captures
    );
    for capture_id in &expected_captures {
        expected_jobs.push(cleanup.enqueue(&scope, *capture_id).await.unwrap());
    }
    expected_jobs.sort();
    assert!(due_ids(&cleanup, &scope, cutoff).await.is_empty());
    assert_eq!(due_ids(&cleanup, &scope, Utc::now()).await, expected_jobs);
    // Claiming an exact later row must not consume the first row.
    let exact_id = expected_jobs[2];
    let exact = cleanup.claim(&scope, exact_id).await.unwrap().unwrap();
    assert!(exact.retained_message_inventory_sha256.is_none());
    assert_eq!(exact.cleanup_id, exact_id);
    assert!(cleanup.claim(&scope, exact_id).await.unwrap().is_none());
    assert_eq!(
        due_ids(&cleanup, &scope, Utc::now()).await,
        expected_jobs[..2]
    );

    let reservation_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO channel_account_preflight_reservations \
         (operator_id,account_id,reservation_id,expires_at) VALUES ($1,$2,$3,now()+interval '2 minutes')",
    ).bind(operator).bind(account).bind(reservation_id)
        .execute(database.pool()).await.unwrap();
    // A perfectly valid scheduling lease and account lock are insufficient
    // when the retained raw conversation lacks explicit completion evidence.
    assert!(
        cleanup
            .authorize_delete(&scope, exact_id, exact.lease_id, reservation_id)
            .await
            .is_err()
    );
    let source_json = r#"{"messages":[{"chat":{"id":"synthetic-complete"}},{"message":{"id":"synthetic-user","chat_id":"synthetic-complete","role":"user"}},{"message":{"id":"synthetic-message","chat_id":"synthetic-complete","role":"assistant","status":"COMPLETED"}}]}"#.to_owned();
    let complete = ObservationCaptureInput {
        capture_id: Uuid::new_v4(),
        runner_session_id: Uuid::new_v4(),
        snapshot: ObservationCaptureSnapshot::Source {
            source_sha256: sha256_hex(source_json.as_bytes()),
            source_json,
        },
        owned_conversation: Some(CapturedConversation {
            provider: "kimi".into(),
            external_conversation_id: "synthetic-complete".into(),
            purpose: CapturedConversationPurpose::Measurement,
            correlation: ConversationCorrelation::CreateResponse,
        }),
        completion: Some(geo_domain::ObservationCaptureCompletion {
            protocol: geo_domain::ObservationCompletionProtocol::ConnectJson,
            terminal: true,
            assistant_message_ids: vec!["synthetic-message".into()],
        }),
        ..input.clone()
    };
    captures.save(&scope, complete.clone()).await.unwrap();
    let complete_id = cleanup.enqueue(&scope, complete.capture_id).await.unwrap();
    let complete_claim = cleanup.claim(&scope, complete_id).await.unwrap().unwrap();
    assert_eq!(
        complete_claim.retained_message_inventory_sha256,
        Some(sha256_hex(
            br#"[["synthetic-message","assistant"],["synthetic-user","user"]]"#
        ))
    );
    assert!(
        cleanup
            .authorize_delete(&scope, complete_id, complete_claim.lease_id, Uuid::new_v4())
            .await
            .is_err()
    );
    assert!(
        cleanup
            .authorize_delete(
                &other_scope,
                complete_id,
                complete_claim.lease_id,
                reservation_id
            )
            .await
            .is_err()
    );
    sqlx::query("UPDATE projects SET status='paused' WHERE project_id=$1")
        .bind(project)
        .execute(database.pool())
        .await
        .unwrap();
    assert_eq!(
        cleanup
            .authorize_delete(&scope, complete_id, complete_claim.lease_id, reservation_id)
            .await
            .unwrap(),
        complete_claim,
    );
    sqlx::query("UPDATE channel_account_preflight_reservations SET expires_at=now()-interval '1 second' WHERE reservation_id=$1")
        .bind(reservation_id).execute(database.pool()).await.unwrap();
    assert!(
        cleanup
            .authorize_delete(&scope, complete_id, complete_claim.lease_id, reservation_id)
            .await
            .is_err()
    );
    sqlx::query("UPDATE channel_account_preflight_reservations SET expires_at=now()+interval '2 minutes' WHERE reservation_id=$1")
        .bind(reservation_id).execute(database.pool()).await.unwrap();
    sqlx::query(
        "UPDATE channel_execution_attempts SET received_at=NULL,outcome=NULL WHERE attempt_id=$1",
    )
    .bind(attempt)
    .execute(database.pool())
    .await
    .unwrap();
    assert!(
        cleanup
            .authorize_delete(&scope, complete_id, complete_claim.lease_id, reservation_id)
            .await
            .is_err()
    );
    sqlx::query("UPDATE channel_execution_attempts SET received_at=now(),outcome='{\"status\":\"unknown\"}' WHERE attempt_id=$1")
        .bind(attempt).execute(database.pool()).await.unwrap();
    sqlx::query("UPDATE provider_conversation_cleanup SET lease_until=now()-interval '1 second' WHERE cleanup_id=$1")
        .bind(complete_id).execute(database.pool()).await.unwrap();
    assert!(
        cleanup
            .authorize_delete(&scope, complete_id, complete_claim.lease_id, reservation_id)
            .await
            .is_err()
    );
    assert!(
        due_ids(&cleanup, &scope, Utc::now())
            .await
            .contains(&complete_id)
    );
    let expired = cleanup.claim(&scope, complete_id).await.unwrap().unwrap();
    assert_eq!(expired.action, ProviderCleanupAction::Reconcile);
    assert!(
        cleanup
            .authorize_delete(&scope, complete_id, expired.lease_id, reservation_id)
            .await
            .is_err()
    );

    // A second project's claim to the same exact provider/account/chat must
    // neither create another cleanup row nor allow the original to be claimed.
    let second_project = other_scope.project_id.unwrap().as_uuid();
    let second_plan = Uuid::new_v4();
    let second_target = Uuid::new_v4();
    let second_attempt = Uuid::new_v4();
    sqlx::query("INSERT INTO projects(project_id,operator_id,tenant_id,slug,display_name) VALUES($1,$2,$3,$4,'Synthetic')")
        .bind(second_project).bind(operator).bind(tenant).bind(format!("cleanup-{second_project}"))
        .execute(database.pool()).await.unwrap();
    sqlx::query("INSERT INTO measurement_execution_plans(plan_id,operator_id,tenant_id,project_id,idempotency_key,request_hash,input_hash,revision,plan,created_at) VALUES($1,$2,$3,$4,$5,'request','frozen',1,'{}',now())")
        .bind(second_plan).bind(operator).bind(tenant).bind(second_project).bind(format!("cleanup-{second_plan}"))
        .execute(database.pool()).await.unwrap();
    sqlx::query("INSERT INTO channel_execution_targets(target_id,operator_id,tenant_id,project_id,kind,frozen_input,ordinal,measurement_plan_id) SELECT $1,operator_id,tenant_id,$2,kind,frozen_input,0,$3 FROM channel_execution_targets WHERE target_id=$4")
        .bind(second_target).bind(second_project).bind(second_plan).bind(target)
        .execute(database.pool()).await.unwrap();
    sqlx::query("INSERT INTO channel_execution_attempts(attempt_id,operator_id,tenant_id,project_id,target_id,account_id,target_kind,claimed_at,received_at,outcome) VALUES($1,$2,$3,$4,$5,$6,'measure',now(),now(),'{\"status\":\"unknown\"}')")
        .bind(second_attempt).bind(operator).bind(tenant).bind(second_project).bind(second_target).bind(account)
        .execute(database.pool()).await.unwrap();
    let duplicated_input = captures
        .get(&scope, expected_captures[0])
        .await
        .unwrap()
        .unwrap()
        .input;
    let cross_project = ObservationCaptureInput {
        capture_id: Uuid::new_v4(),
        target_id: second_target,
        attempt_id: second_attempt,
        runner_session_id: Uuid::new_v4(),
        ..duplicated_input
    };
    captures
        .save(&other_scope, cross_project.clone())
        .await
        .unwrap();
    assert!(
        backfill_ids(&cleanup, &other_scope, Utc::now())
            .await
            .is_empty()
    );
    assert!(
        cleanup
            .enqueue(&other_scope, cross_project.capture_id)
            .await
            .is_err()
    );
    let ambiguous_id = cleanup.enqueue(&scope, expected_captures[0]).await.unwrap();
    // This row may be the exact row claimed earlier; release it to pending
    // without removing its immutable ownership proof.
    sqlx::query("UPDATE provider_conversation_cleanup SET state='pending',lease_id=NULL,lease_until=NULL,next_attempt_at=now() WHERE cleanup_id=$1")
        .bind(ambiguous_id).execute(database.pool()).await.unwrap();
    assert!(cleanup.claim(&scope, ambiguous_id).await.unwrap().is_none());
}

async fn backfill_ids(
    repository: &PgProviderConversationCleanupRepository,
    scope: &TenantScope,
    as_of: chrono::DateTime<Utc>,
) -> Vec<Uuid> {
    let mut after = None;
    let mut ids = Vec::new();
    loop {
        let page = repository.scan_unqueued(as_of, after, 1).await.unwrap();
        assert!(page.len() <= 1);
        let Some(item) = page.first() else { break };
        assert!(after.is_none_or(|previous| item.capture_id > previous));
        after = Some(item.capture_id);
        if item.scope == *scope {
            ids.push(item.capture_id);
        }
    }
    ids
}

async fn due_ids(
    repository: &PgProviderConversationCleanupRepository,
    scope: &TenantScope,
    as_of: chrono::DateTime<Utc>,
) -> Vec<Uuid> {
    let mut after = None;
    let mut ids = Vec::new();
    loop {
        let page = repository.scan_due(as_of, after, 1).await.unwrap();
        assert!(page.len() <= 1);
        let Some(item) = page.first() else { break };
        assert!(after.is_none_or(|previous| item.cleanup_id > previous));
        after = Some(item.cleanup_id);
        if item.scope == *scope {
            ids.push(item.cleanup_id);
        }
    }
    ids
}
