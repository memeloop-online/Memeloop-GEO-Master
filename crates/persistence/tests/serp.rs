use chrono::{DateTime, Duration, Utc};
use geo_domain::*;
use geo_persistence::{Database, DatabaseConfig, MemorySerpRepository, PgSerpRepository};
use uuid::Uuid;

fn measurement(now: DateTime<Utc>) -> SerpMeasurement {
    SerpMeasurement {
        measurement_id: Uuid::new_v4(),
        source_key: "synthetic-primary".into(),
        protocol: SerpProtocol {
            query: "  café + rainfall%  ".into(),
            engine: SerpEngine::Google,
            surface: SerpSurface::ThirdPartyApi,
            source: "synthetic".into(),
            source_location_code: "2840".into(),
            country: "US".into(),
            city: None,
            language: "en".into(),
            device: SerpDevice::Desktop,
            operating_system: "windows".into(),
            requested_depth: 10,
            max_pages: 1,
            priority: 1,
            login: "unspecified".into(),
            personalization: "unspecified".into(),
            protocol_version: SERP_PROTOCOL_VERSION.into(),
            connector_version: "synthetic.v1".into(),
        },
        target: None,
        target_rule_version: SERP_TARGET_RULE_VERSION.into(),
        question_binding: None,
        scheduled_at: now,
        created_at: now,
        state: SerpTaskState::Queued,
    }
}

fn raw(
    intent: &SerpSendingIntent,
    operation: SerpEvidenceOperation,
    now: DateTime<Utc>,
) -> SerpRawEvidence {
    let body = br#"{"id":"synthetic-task","status":"ready"}"#.to_vec();
    SerpRawEvidence {
        evidence_id: Uuid::new_v4(),
        measurement_id: intent.measurement_id,
        attempt_id: intent.attempt_id,
        operation,
        provider_task_id: (operation != SerpEvidenceOperation::Submission)
            .then(|| "synthetic-task".into()),
        request_sha256: if operation == SerpEvidenceOperation::Submission {
            intent.request_sha256.clone()
        } else {
            sha256_hex(b"synthetic-task-get")
        },
        intent_request_sha256: intent.request_sha256.clone(),
        response_sha256: sha256_hex(&body),
        body,
        body_complete: true,
        http_status: Some(200),
        send_certainty: SerpSendCertainty::ResponseReceived,
        captured_at: now,
    }
}

fn task(intent: &SerpSendingIntent, evidence_id: Uuid) -> SerpProviderTask {
    SerpProviderTask {
        measurement_id: intent.measurement_id,
        attempt_id: intent.attempt_id,
        binding_evidence_id: evidence_id,
        provider_task_id: "synthetic-task".into(),
        correlation_tag: intent.correlation_tag.clone(),
    }
}

fn observation(raw: &SerpStoredRaw, now: DateTime<Utc>) -> SerpObservation {
    SerpObservation {
        observation_id: Uuid::new_v4(),
        measurement_id: raw.evidence.measurement_id,
        attempt_id: raw.evidence.attempt_id,
        raw_evidence_id: raw.evidence.evidence_id,
        raw_sha256: raw.evidence.response_sha256.clone(),
        parser_version: "synthetic.v1".into(),
        provider_observed_at: None,
        received_at: raw.evidence.captured_at,
        analyzed_at: now.max(raw.stored_at),
        status: SerpObservationStatus::Partial,
        actual_conditions: SerpActualConditions::default(),
        coverage: SerpCoverage {
            requested_depth: 10,
            observed_organic_depth: 0,
            pages_received: 1,
            completion: SerpCoverageCompletion::Partial,
            truncated: true,
            exhaustion_evidence_locator: None,
        },
        results: vec![],
        source_limitations: vec![SerpLimitationCode::ResultUnavailable],
    }
}

async fn contract(
    store: &dyn SerpRepository,
    scope: &TenantScope,
    other: &TenantScope,
    now: DateTime<Utc>,
) {
    let first = measurement(now);
    let id = first.measurement_id;
    assert_eq!(
        store.accept(scope, "first", first.clone()).await.unwrap(),
        first
    );
    let execution = store.get_execution(scope, id).await.unwrap().unwrap();
    assert!(execution.intent.is_none() && execution.claim.is_none());
    assert_eq!(
        store.list_due(scope, now, None, 100).await.unwrap(),
        vec![first.clone()]
    );
    assert!(store.get_execution(other, id).await.unwrap().is_none());
    let mut replay = first.clone();
    replay.measurement_id = Uuid::new_v4();
    replay.created_at += Duration::seconds(1);
    assert_eq!(
        store.accept(scope, "first", replay.clone()).await.unwrap(),
        first
    );
    replay.protocol.query = replay.protocol.query.trim().into();
    assert_eq!(
        store.accept(scope, "first", replay).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    let mut rerouted = first.clone();
    rerouted.measurement_id = Uuid::new_v4();
    rerouted.source_key = "synthetic-secondary".into();
    assert_eq!(
        store
            .accept(scope, "first", rerouted)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert!(store.get(other, id).await.unwrap().is_none());
    assert!(
        store
            .claim(other, id, now, now + Duration::minutes(1))
            .await
            .is_err()
    );
    let (a, b) = tokio::join!(
        store.claim(scope, id, now, now + Duration::minutes(1)),
        store.claim(scope, id, now, now + Duration::minutes(1))
    );
    let mut claims: Vec<_> = [a.unwrap(), b.unwrap()].into_iter().flatten().collect();
    assert_eq!(claims.len(), 1);
    let claim = claims.remove(0);
    assert!(
        store
            .list_due(scope, now, None, 100)
            .await
            .unwrap()
            .is_empty()
    );
    let renewed = store
        .renew_claim(scope, &claim, now, now + Duration::minutes(2))
        .await
        .unwrap();
    let request_sha = sha256_hex(b"synthetic-paid-request");
    let (a, b) = tokio::join!(
        store.begin_send(
            scope,
            &renewed,
            &request_sha,
            "opaque-synthetic-tag",
            None,
            now
        ),
        store.begin_send(
            scope,
            &renewed,
            &request_sha,
            "opaque-synthetic-tag",
            None,
            now
        )
    );
    let mut authorizations: Vec<_> = [a.unwrap(), b.unwrap()].into_iter().flatten().collect();
    assert_eq!(authorizations.len(), 1);
    let intent = authorizations.remove(0);
    assert_eq!(intent.credential_revision, None);
    assert!(
        store
            .begin_send(
                scope,
                &renewed,
                &request_sha,
                "opaque-synthetic-tag",
                Some(2),
                now
            )
            .await
            .is_err()
    );
    assert_eq!(
        store
            .get_execution(scope, id)
            .await
            .unwrap()
            .unwrap()
            .intent,
        Some(intent.clone())
    );
    assert!(
        store
            .begin_send(
                scope,
                &renewed,
                &request_sha,
                "opaque-synthetic-tag",
                None,
                now
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .begin_send(
                scope,
                &renewed,
                &sha256_hex(b"changed"),
                "opaque-synthetic-tag",
                None,
                now
            )
            .await
            .is_err()
    );
    assert_eq!(
        store
            .get_sending_intent(scope, id, claim.attempt_id)
            .await
            .unwrap(),
        Some(intent.clone())
    );
    assert!(
        store
            .get_sending_intent(scope, id, Uuid::new_v4())
            .await
            .unwrap()
            .is_none()
    );
    let receipt = raw(&intent, SerpEvidenceOperation::Submission, now);
    let persisted = store
        .append_raw(scope, &intent, receipt.clone())
        .await
        .unwrap();
    assert_eq!(persisted.evidence, receipt);
    assert_eq!(
        store
            .append_raw(scope, &intent, receipt.clone())
            .await
            .unwrap(),
        persisted
    );
    let mut malformed = raw(&intent, SerpEvidenceOperation::Submission, now);
    malformed.body = b"{malformed".to_vec();
    malformed.response_sha256 = sha256_hex(&malformed.body);
    let malformed = store.append_raw(scope, &intent, malformed).await.unwrap();
    let receipts = store.list_raw(scope, id, None, 100).await.unwrap();
    assert_eq!(receipts.len(), 2);
    assert_eq!(receipts[0], malformed.receipt());
    assert_eq!(
        store
            .list_raw(scope, id, Some(receipts[0].evidence_id), 1)
            .await
            .unwrap(),
        vec![persisted.receipt()]
    );
    assert!(
        store
            .list_raw(other, id, Some(receipts[0].evidence_id), 1)
            .await
            .is_err()
    );
    assert!(
        store
            .list_raw(scope, id, Some(Uuid::new_v4()), 1)
            .await
            .is_err()
    );
    assert!(store.list_raw(scope, id, None, 101).await.is_err());
    let receipt_json = serde_json::to_value(&receipts[0]).unwrap();
    assert!(receipt_json.get("body").is_none() && receipt_json.get("send_token").is_none());
    assert!(store.get_execution(other, id).await.unwrap().is_none());
    assert!(
        store
            .get_raw(other, receipt.evidence_id)
            .await
            .unwrap()
            .is_none()
    );
    for foreign in [
        TenantScope::new(Uuid::new_v4().into(), scope.tenant_id, scope.project_id),
        TenantScope::new(scope.operator_id, Uuid::new_v4().into(), scope.project_id),
    ] {
        assert!(store.get(&foreign, id).await.unwrap().is_none());
        assert!(
            store
                .get_raw(&foreign, receipt.evidence_id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(store.list(&foreign, None, 10).await.unwrap().is_empty());
        assert!(
            store
                .append_raw(&foreign, &intent, receipt.clone())
                .await
                .is_err()
        );
    }
    let mut forged = intent.clone();
    forged.send_token = Uuid::new_v4();
    assert!(
        store
            .append_raw(scope, &forged, receipt.clone())
            .await
            .is_err()
    );
    let mut altered = receipt.clone();
    altered.body.push(b' ');
    altered.response_sha256 = sha256_hex(&altered.body);
    assert_eq!(
        store
            .append_raw(scope, &intent, altered)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let binding = task(&intent, receipt.evidence_id);
    assert_eq!(
        store
            .bind_provider_task(scope, &intent, binding.clone())
            .await
            .unwrap(),
        binding
    );
    assert_eq!(
        store
            .bind_provider_task(scope, &intent, binding.clone())
            .await
            .unwrap(),
        binding
    );
    let mut other_task = binding.clone();
    other_task.provider_task_id = "different-task".into();
    assert!(
        store
            .bind_provider_task(scope, &intent, other_task)
            .await
            .is_err()
    );
    store
        .finish(scope, &renewed, SerpTaskState::AwaitingResult, now)
        .await
        .unwrap();
    let execution = store.get_execution(scope, id).await.unwrap().unwrap();
    assert!(execution.claim.is_none());
    assert_eq!(execution.next_poll_at, Some(now));
    let read_claim = store
        .claim_read(scope, id, now, now + Duration::minutes(1))
        .await
        .unwrap()
        .unwrap();
    assert!(
        store
            .list_due(scope, now, None, 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .release_read(other, &read_claim, now + Duration::seconds(30), now)
            .await
            .is_err()
    );
    store
        .release_read(scope, &read_claim, now + Duration::seconds(30), now)
        .await
        .unwrap();
    assert!(
        store
            .release_read(scope, &read_claim, now + Duration::seconds(30), now)
            .await
            .is_err()
    );
    assert!(
        store
            .claim_read(
                scope,
                id,
                now + Duration::seconds(29),
                now + Duration::minutes(1)
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .list_due(scope, now + Duration::seconds(29), None, 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .list_due(scope, now + Duration::seconds(30), None, 100)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(store.list_due(other, now, Some(id), 100).await.is_err());
    assert!(store.list_due(scope, now, None, 101).await.is_err());
    store.cancel(scope, id, now).await.unwrap();
    store.cancel(scope, id, now).await.unwrap();
    assert!(
        store
            .renew_claim(scope, &renewed, now, now + Duration::minutes(3))
            .await
            .is_err()
    );
    let late = raw(
        &intent,
        SerpEvidenceOperation::ResultRead,
        now + Duration::seconds(1),
    );
    let late = store.append_raw(scope, &intent, late).await.unwrap();
    let analysis = observation(&late, now + Duration::seconds(2));
    store
        .append_observation(scope, analysis.clone())
        .await
        .unwrap();
    assert_eq!(
        store
            .get_observation(scope, id, analysis.observation_id)
            .await
            .unwrap(),
        Some(analysis.clone())
    );
    assert!(
        store
            .get_observation(other, id, analysis.observation_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_observation(scope, Uuid::new_v4(), analysis.observation_id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .append_observation(scope, analysis.clone())
            .await
            .unwrap(),
        analysis
    );
    let mut changed = analysis.clone();
    changed.parser_version = "synthetic.v2".into();
    assert!(store.append_observation(scope, changed).await.is_err());
    assert!(store.append_observation(other, analysis).await.is_err());
    assert!(
        store
            .finish(scope, &renewed, SerpTaskState::Completed, now)
            .await
            .is_err()
    );
    assert_eq!(
        store.get(scope, id).await.unwrap().unwrap().state,
        SerpTaskState::Cancelled
    );
    let cancelled_recovery = store
        .append_raw(
            scope,
            &intent,
            raw(
                &intent,
                SerpEvidenceOperation::RecoveryRead,
                now + Duration::seconds(1),
            ),
        )
        .await
        .unwrap();
    assert!(
        store
            .recover_provider_task(
                scope,
                &intent,
                task(&intent, cancelled_recovery.evidence.evidence_id),
                now + Duration::seconds(2)
            )
            .await
            .is_err()
    );

    // Lost submission receipt: preserve the intent, archive exact-task recovery
    // bytes, and mint only a read fence. Never authorize another paid submission.
    let mut recovery = measurement(now);
    recovery.source_key = "synthetic-secondary".into();
    assert_eq!(recovery.protocol, first.protocol);
    let recovery_id = recovery.measurement_id;
    store
        .accept(scope, "recovery", recovery.clone())
        .await
        .unwrap();
    assert_eq!(
        store
            .get(scope, recovery_id)
            .await
            .unwrap()
            .unwrap()
            .source_key,
        "synthetic-secondary"
    );
    assert_eq!(
        store.get(scope, id).await.unwrap().unwrap().source_key,
        "synthetic-primary"
    );
    let old = store
        .claim(scope, recovery_id, now, now + Duration::seconds(1))
        .await
        .unwrap()
        .unwrap();
    let recovery_intent = store
        .begin_send(scope, &old, &request_sha, "opaque-recovery-tag", None, now)
        .await
        .unwrap()
        .unwrap();
    let recovery_at = now + Duration::seconds(2);
    assert_eq!(
        store.expire_claims(scope, recovery_at, 100).await.unwrap(),
        vec![recovery_id]
    );
    assert_eq!(
        store.get(scope, recovery_id).await.unwrap().unwrap().state,
        SerpTaskState::Unknown
    );
    let execution = store
        .get_execution(scope, recovery_id)
        .await
        .unwrap()
        .unwrap();
    assert!(execution.claim.is_none());
    assert_eq!(execution.intent, Some(recovery_intent.clone()));
    store
        .defer_recovery(
            scope,
            recovery_id,
            recovery_at + Duration::minutes(5),
            recovery_at,
        )
        .await
        .unwrap();
    store
        .defer_recovery(
            scope,
            recovery_id,
            recovery_at + Duration::minutes(1),
            recovery_at,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .get_execution(scope, recovery_id)
            .await
            .unwrap()
            .unwrap()
            .next_poll_at,
        Some(recovery_at + Duration::minutes(5))
    );
    assert!(
        store
            .list_due(scope, recovery_at, None, 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .list_due(scope, recovery_at + Duration::minutes(5), None, 100)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        store
            .defer_recovery(scope, id, recovery_at, recovery_at)
            .await
            .is_err()
    );
    assert!(
        store
            .claim(
                scope,
                recovery_id,
                recovery_at,
                recovery_at + Duration::minutes(1)
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .begin_send(
                scope,
                &old,
                &request_sha,
                "opaque-recovery-tag",
                None,
                recovery_at
            )
            .await
            .is_err()
    );
    assert!(
        store
            .finish(scope, &old, SerpTaskState::Failed, recovery_at)
            .await
            .is_err()
    );
    let recovery_raw = store
        .append_raw(
            scope,
            &recovery_intent,
            raw(
                &recovery_intent,
                SerpEvidenceOperation::RecoveryRead,
                recovery_at,
            ),
        )
        .await
        .unwrap();
    let recovery_task = task(&recovery_intent, recovery_raw.evidence.evidence_id);
    let mut mismatch = recovery_task.clone();
    mismatch.provider_task_id = "unrelated-task".into();
    assert!(
        store
            .recover_provider_task(scope, &recovery_intent, mismatch, recovery_at)
            .await
            .is_err()
    );
    assert_eq!(
        store
            .recover_provider_task(scope, &recovery_intent, recovery_task.clone(), recovery_at)
            .await
            .unwrap(),
        recovery_task
    );
    let reader = store
        .claim_read(
            scope,
            recovery_id,
            recovery_at,
            recovery_at + Duration::minutes(1),
        )
        .await
        .unwrap()
        .unwrap();
    assert_ne!(reader.claim_token, old.claim_token);
    assert_eq!(reader.attempt_id, old.attempt_id);
    assert!(
        store
            .begin_send(
                scope,
                &reader,
                &request_sha,
                "opaque-recovery-tag",
                None,
                recovery_at
            )
            .await
            .is_err()
    );
    assert!(
        store
            .claim_read(
                scope,
                recovery_id,
                recovery_at,
                recovery_at + Duration::minutes(1)
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .finish(scope, &reader, SerpTaskState::Completed, recovery_at)
            .await
            .is_err()
    );
    let mut versions = [
        observation(&recovery_raw, recovery_at),
        observation(&recovery_raw, recovery_at),
    ];
    versions.sort_by_key(|value| value.observation_id);
    for version in &versions {
        store
            .append_observation(scope, version.clone())
            .await
            .unwrap();
    }
    let page = store
        .list_observations(scope, recovery_id, None, 1)
        .await
        .unwrap();
    assert_eq!(page, vec![versions[1].clone()]);
    assert_eq!(
        store
            .list_observations(scope, recovery_id, Some(page[0].observation_id), 1)
            .await
            .unwrap(),
        vec![versions[0].clone()]
    );
    assert!(
        store
            .list_observations(other, recovery_id, Some(page[0].observation_id), 1)
            .await
            .is_err()
    );
    assert!(
        store
            .list_observations(scope, id, Some(page[0].observation_id), 1)
            .await
            .is_err()
    );
    let expired_at = reader.lease_expires_at;
    assert_eq!(
        store.expire_claims(scope, expired_at, 100).await.unwrap(),
        vec![recovery_id]
    );
    let new_proof = store
        .append_raw(
            scope,
            &recovery_intent,
            raw(
                &recovery_intent,
                SerpEvidenceOperation::RecoveryRead,
                expired_at,
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .recover_provider_task(
                scope,
                &recovery_intent,
                task(&recovery_intent, new_proof.evidence.evidence_id),
                expired_at
            )
            .await
            .unwrap(),
        recovery_task,
        "read recovery preserves the original immutable external task binding"
    );
    let next_reader = store
        .claim_read(
            scope,
            recovery_id,
            expired_at,
            expired_at + Duration::minutes(1),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        store
            .finish(scope, &reader, SerpTaskState::Completed, expired_at)
            .await
            .is_err()
    );
    store
        .finish(scope, &next_reader, SerpTaskState::Completed, expired_at)
        .await
        .unwrap();
    assert_eq!(
        store.get(scope, recovery_id).await.unwrap().unwrap().state,
        SerpTaskState::Completed
    );
    assert!(
        store
            .claim(
                scope,
                recovery_id,
                recovery_at,
                recovery_at + Duration::minutes(1)
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .expire_claims(scope, recovery_at + Duration::hours(1), 100)
            .await
            .unwrap()
            .is_empty()
    );

    // A crash before begin_send is the only safe route back to a sendable queue.
    let queued = measurement(now);
    let queued_id = queued.measurement_id;
    store.accept(scope, "unsent", queued).await.unwrap();
    let obsolete = store
        .claim(scope, queued_id, now, now + Duration::seconds(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store.expire_claims(scope, recovery_at, 1).await.unwrap(),
        vec![queued_id]
    );
    let fresh = store
        .claim(
            scope,
            queued_id,
            recovery_at,
            recovery_at + Duration::minutes(1),
        )
        .await
        .unwrap()
        .unwrap();
    assert_ne!(obsolete.attempt_id, fresh.attempt_id);
    assert!(
        store
            .begin_send(
                scope,
                &obsolete,
                &request_sha,
                "stale-tag",
                None,
                recovery_at
            )
            .await
            .is_err()
    );
    store.cancel(scope, queued_id, recovery_at).await.unwrap();
    let rows = store.list(scope, None, 100).await.unwrap();
    assert_eq!(rows.len(), 3);
    let one = store.list(scope, None, 1).await.unwrap();
    let rest = store
        .list(scope, Some(one[0].measurement_id), 100)
        .await
        .unwrap();
    assert_eq!(rest, rows[1..]);
    assert!(
        store
            .list(other, Some(one[0].measurement_id), 100)
            .await
            .is_err()
    );
    assert!(store.list(scope, None, 101).await.is_err());
    let no_project = TenantScope::new(scope.operator_id, scope.tenant_id, None);
    assert_eq!(
        store.list(&no_project, None, 1).await.unwrap_err().code,
        ErrorCode::Forbidden
    );
}

#[tokio::test]
async fn memory_serp_fences_recovery_and_immutable_evidence() {
    let scope = TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    );
    let other = TenantScope::new(
        scope.operator_id,
        scope.tenant_id,
        Some(Uuid::new_v4().into()),
    );
    let now = DateTime::from_timestamp(Utc::now().timestamp() - 10, 0).unwrap();
    contract(&MemorySerpRepository::default(), &scope, &other, now).await;
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn postgres_serp_fences_recovery_and_immutable_evidence() {
    let config = DatabaseConfig::from_url(
        std::env::var("GEO_TEST_DATABASE_URL").expect("disposable PostgreSQL URL required"),
    )
    .unwrap();
    let database = Database::connect_and_migrate(&config).await.unwrap();
    let (operator, tenant, project, other_project) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    sqlx::query("INSERT INTO operators(operator_id,slug,display_name) VALUES($1,$2,'Synthetic')")
        .bind(operator)
        .bind(format!("serp-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants(tenant_id,operator_id,slug,display_name) VALUES($1,$2,$3,'Synthetic')",
    )
    .bind(tenant)
    .bind(operator)
    .bind(format!("serp-{tenant}"))
    .execute(database.pool())
    .await
    .unwrap();
    for id in [project, other_project] {
        sqlx::query("INSERT INTO projects(project_id,operator_id,tenant_id,slug,display_name) VALUES($1,$2,$3,$4,'Synthetic')")
            .bind(id).bind(operator).bind(tenant).bind(format!("serp-{id}")).execute(database.pool()).await.unwrap();
    }
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.into()));
    let other = TenantScope::new(operator.into(), tenant.into(), Some(other_project.into()));
    let database_now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(database.pool())
        .await
        .unwrap();
    let now = DateTime::from_timestamp(database_now.timestamp() - 10, 0).unwrap();
    contract(
        &PgSerpRepository::from_database(&database),
        &scope,
        &other,
        now,
    )
    .await;
    let restarted = PgSerpRepository::from_database(&database);
    let restored = restarted.list(&scope, None, 100).await.unwrap();
    assert_eq!(restored.len(), 3);
    assert_eq!(
        restored
            .iter()
            .filter(|row| row.source_key == "synthetic-secondary")
            .count(),
        1
    );
    assert_eq!(
        restored
            .iter()
            .filter(|row| row.source_key == "synthetic-primary")
            .count(),
        2
    );
    assert!(restarted.list(&other, None, 100).await.unwrap().is_empty());
}

async fn report_now(database: Option<&Database>) -> DateTime<Utc> {
    match database {
        Some(database) => sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(database.pool())
            .await
            .unwrap(),
        None => Utc::now(),
    }
}

async fn report_contract(
    store: &dyn SerpRepository,
    scope: &TenantScope,
    database: Option<&Database>,
) {
    // The database owns insertion clocks; remote test hosts need not share the
    // client clock. Never compensate for skew with sleeps or a future cutoff.
    let now = report_now(database).await - Duration::minutes(2);
    let window = MeasurementPeriodWindow {
        start_at: now - Duration::minutes(1),
        end_at: now + Duration::minutes(1),
        report_timezone: "UTC".into(),
    };
    let mut expected = Vec::new();
    for ordinal in 1..=105u128 {
        let mut item = measurement(now);
        item.measurement_id = Uuid::from_u128(ordinal);
        expected.push(item.measurement_id);
        store
            .accept(scope, &format!("report-{ordinal}"), item)
            .await
            .unwrap();
    }
    let cutoff = report_now(database).await;
    let first = store
        .list_report_measurements(scope, &window, cutoff, None, 100)
        .await
        .unwrap();
    assert_eq!(first.len(), 100);
    let second = store
        .list_report_measurements(scope, &window, cutoff, Some(first[99].measurement_id), 100)
        .await
        .unwrap();
    assert_eq!(second.len(), 5);
    assert_eq!(
        first
            .iter()
            .chain(&second)
            .map(|row| row.measurement_id)
            .collect::<Vec<_>>(),
        expected
    );
    assert!(
        first
            .iter()
            .all(|row| row.created_at < row.stored_at && row.stored_at <= cutoff)
    );
    assert!(
        store
            .list_report_measurements(scope, &window, now + Duration::minutes(1), None, 100)
            .await
            .unwrap()
            .is_empty()
    );

    let id = expected[0];
    let frozen = first[0].clone();
    let original = store.get(scope, id).await.unwrap().unwrap();
    let mut replay = original.clone();
    replay.measurement_id = Uuid::new_v4();
    replay.created_at = report_now(database).await;
    store.accept(scope, "report-1", replay).await.unwrap();
    assert_eq!(
        store
            .get_report_measurements(scope, &[id, id, Uuid::new_v4()])
            .await
            .unwrap(),
        vec![frozen.clone()]
    );
    for (key, scheduled_at) in [
        ("before-window", window.start_at - Duration::microseconds(1)),
        ("end-window", window.end_at),
        ("late-insertion", now),
    ] {
        let mut item = measurement(now);
        item.scheduled_at = scheduled_at;
        store.accept(scope, key, item).await.unwrap();
    }
    // A backdated accepted task cannot enter an already fixed cohort cutoff.
    assert_eq!(
        store
            .list_report_measurements(scope, &window, cutoff, None, 100)
            .await
            .unwrap(),
        first
    );
    assert_eq!(
        store
            .list_report_measurements(scope, &window, cutoff, Some(first[99].measurement_id), 100)
            .await
            .unwrap(),
        second
    );
    let later = store
        .list_report_measurements(
            scope,
            &window,
            report_now(database).await,
            Some(Uuid::from_u128(105)),
            100,
        )
        .await
        .unwrap();
    assert_eq!(later.len(), 1);
    assert_eq!(later[0].scheduled_at, now);

    let claim = store
        .claim(scope, id, now, now + Duration::minutes(10))
        .await
        .unwrap()
        .unwrap();
    let intent = store
        .begin_send(
            scope,
            &claim,
            &sha256_hex(b"report-request"),
            "report-tag",
            None,
            now,
        )
        .await
        .unwrap()
        .unwrap();
    let submission = store
        .append_raw(
            scope,
            &intent,
            raw(&intent, SerpEvidenceOperation::Submission, now),
        )
        .await
        .unwrap();
    store
        .bind_provider_task(
            scope,
            &intent,
            task(&intent, submission.evidence.evidence_id),
        )
        .await
        .unwrap();
    let saved = store
        .append_raw(
            scope,
            &intent,
            raw(&intent, SerpEvidenceOperation::ResultRead, now),
        )
        .await
        .unwrap();
    let mut obs = observation(&saved, report_now(database).await);
    obs.observation_id = Uuid::from_u128(1000);
    store.append_observation(scope, obs.clone()).await.unwrap();
    let prior = store
        .list_report_observations(scope, &[id], report_now(database).await, None, 100)
        .await
        .unwrap();
    assert_eq!(prior.len(), 1);
    assert_eq!(prior[0].raw.stored_at, saved.stored_at);
    assert!(prior[0].observation_stored_at >= obs.analyzed_at);
    let observation_cutoff = prior[0].observation_stored_at;
    store
        .append_raw(scope, &intent, saved.evidence.clone())
        .await
        .unwrap();
    store.append_observation(scope, obs.clone()).await.unwrap();
    assert_eq!(
        store
            .list_report_observations(scope, &[id], report_now(database).await, None, 100)
            .await
            .unwrap(),
        prior
    );
    assert!(
        store
            .list_report_observations(
                scope,
                &[id],
                saved.stored_at - Duration::microseconds(1),
                None,
                100
            )
            .await
            .unwrap()
            .is_empty()
    );
    for ordinal in 1..105u128 {
        let mut next = obs.clone();
        next.observation_id = Uuid::from_u128(1000 + ordinal);
        next.analyzed_at = report_now(database).await;
        store.append_observation(scope, next).await.unwrap();
    }
    assert_eq!(
        store
            .list_report_observations(scope, &[id], observation_cutoff, None, 100)
            .await
            .unwrap(),
        prior
    );
    let first_observations = store
        .list_report_observations(scope, &[id], report_now(database).await, None, 100)
        .await
        .unwrap();
    let rest = store
        .list_report_observations(
            scope,
            &[id],
            report_now(database).await,
            Some(first_observations[99].observation.observation_id),
            100,
        )
        .await
        .unwrap();
    assert_eq!(first_observations.len(), 100);
    assert_eq!(rest.len(), 5);
    assert_eq!(
        first_observations
            .iter()
            .chain(&rest)
            .map(|row| row.observation.observation_id)
            .collect::<Vec<_>>(),
        (1000..1105).map(Uuid::from_u128).collect::<Vec<_>>()
    );
    let serialized = serde_json::to_string(&first_observations).unwrap();
    assert!(!serialized.contains("synthetic-task"));
    assert!(!serialized.contains("\"body\":"));
    assert_eq!(
        store.get_report_measurements(scope, &[id]).await.unwrap(),
        vec![frozen]
    );

    for foreign in [
        TenantScope::new(Uuid::new_v4().into(), scope.tenant_id, scope.project_id),
        TenantScope::new(scope.operator_id, Uuid::new_v4().into(), scope.project_id),
        TenantScope::new(
            scope.operator_id,
            scope.tenant_id,
            Some(Uuid::new_v4().into()),
        ),
    ] {
        assert!(
            store
                .list_report_measurements(&foreign, &window, report_now(database).await, None, 100)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .get_report_measurements(&foreign, &[id])
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .list_report_observations(&foreign, &[id], report_now(database).await, None, 100)
                .await
                .unwrap()
                .is_empty()
        );
    }
    for limit in [0, 101] {
        assert!(
            store
                .list_report_measurements(scope, &window, cutoff, None, limit)
                .await
                .is_err()
        );
        assert!(
            store
                .list_report_observations(scope, &[id], cutoff, None, limit)
                .await
                .is_err()
        );
    }
    assert!(
        store
            .get_report_measurements(scope, &expected)
            .await
            .is_err()
    );
    assert!(
        store
            .list_report_observations(scope, &expected, cutoff, None, 100)
            .await
            .is_err()
    );
    let unscoped = TenantScope::new(scope.operator_id, scope.tenant_id, None);
    assert!(
        store
            .get_report_measurements(&unscoped, &[id])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn memory_serp_report_metadata_scope_cutoff_pagination_and_replay() {
    let scope = TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    );
    report_contract(&MemorySerpRepository::default(), &scope, None).await;
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn postgres_serp_report_metadata_scope_cutoff_pagination_and_replay() {
    let url =
        std::env::var("GEO_TEST_DATABASE_URL").expect("disposable test database URL required");
    let admin = sqlx::PgPool::connect(&url).await.unwrap();
    let schema = format!("serp_report_test_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let options = url
        .parse::<sqlx::postgres::PgConnectOptions>()
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_with(options)
        .await
        .unwrap();
    let database = Database::from_pool(pool);
    database.migrate().await.unwrap();
    let (operator, tenant, project) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    sqlx::query("INSERT INTO operators(operator_id,slug,display_name) VALUES($1,$2,'Synthetic')")
        .bind(operator)
        .bind(format!("report-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants(tenant_id,operator_id,slug,display_name) VALUES($1,$2,$3,'Synthetic')",
    )
    .bind(tenant)
    .bind(operator)
    .bind(format!("report-{tenant}"))
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO projects(project_id,operator_id,tenant_id,slug,display_name) VALUES($1,$2,$3,$4,'Synthetic')")
        .bind(project).bind(operator).bind(tenant).bind(format!("report-{project}")).execute(database.pool()).await.unwrap();
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.into()));
    let repo = PgSerpRepository::from_database(&database);
    report_contract(&repo, &scope, Some(&database)).await;
    let rows = repo
        .get_report_measurements(&scope, &[Uuid::from_u128(1)])
        .await
        .unwrap();
    assert_eq!(
        rows,
        PgSerpRepository::from_database(&database)
            .get_report_measurements(&scope, &[Uuid::from_u128(1)])
            .await
            .unwrap()
    );
    // Storage errors must propagate, never become an empty report section.
    database.pool().close().await;
    assert_eq!(
        repo.get_report_measurements(&scope, &[Uuid::from_u128(1)])
            .await
            .unwrap_err()
            .code,
        ErrorCode::DependencyUnavailable
    );
    // Generated private test schema only; no shared tables are dropped.
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
