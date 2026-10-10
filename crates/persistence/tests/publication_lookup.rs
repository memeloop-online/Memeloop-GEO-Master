//! Explicitly run against GEO_TEST_DATABASE_URL pointing at disposable PostgreSQL.
use chrono::{Duration, Utc};
use geo_domain::{
    ChannelJobRepository, ChannelOutcome, ChannelOutcomeStatus, ChannelPlan, ChannelTarget,
    ChannelTargetInput, ErrorCode, InitialSource, InitialSourceKind, InitialSourceVisibility,
    ProjectCreate, ProjectRepository, ProjectSettings, ProjectStartCommand,
    PublicationLookupFinding, PublicationLookupObservation, PublicationLookupRepository,
    TenantScope, hash_idempotency_key, settings_hash, start_request_hash,
};
use geo_persistence::{
    Database, PgChannelJobRepository, PgProjectRepository, PgPublicationLookupRepository,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

async fn create_scope(database: &Database) -> (TenantScope, Uuid) {
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    sqlx::query("INSERT INTO operators(operator_id,slug,display_name) VALUES($1,$2,'Fixture')")
        .bind(operator)
        .bind(format!("lookup-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants(tenant_id,operator_id,slug,display_name) VALUES($1,$2,$3,'Fixture')",
    )
    .bind(tenant)
    .bind(operator)
    .bind(format!("lookup-{tenant}"))
    .execute(database.pool())
    .await
    .unwrap();
    let owner = TenantScope::new(operator.into(), tenant.into(), None);
    let projects = PgProjectRepository::from_database(database);
    let project = projects
        .create(
            &owner,
            ProjectCreate {
                slug: None,
                display_name: "Fixture".into(),
                settings: ProjectSettings {
                    brand_name: "Fixture".into(),
                    market: "US".into(),
                    language: "en".into(),
                    initial_sources: vec![InitialSource {
                        kind: InitialSourceKind::Url,
                        value: "https://example.invalid/source".into(),
                        visibility: InitialSourceVisibility::Public,
                        version_ref: None,
                        content_hash: None,
                    }],
                    ..ProjectSettings::default()
                },
            },
        )
        .await
        .unwrap();
    let hash = settings_hash(&project.settings.clone().validate_start().unwrap()).unwrap();
    let started = projects
        .start(
            &owner,
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("lookup-fixture"),
                request_hash: start_request_hash(project.id, project.revision, &hash),
                settings_hash: hash,
                operation_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    (
        TenantScope::new(operator.into(), tenant.into(), Some(project.id)),
        started.cycle_id,
    )
}

fn outcome(
    status: ChannelOutcomeStatus,
    at: chrono::DateTime<Utc>,
    proofs: Vec<serde_json::Value>,
) -> ChannelOutcome {
    ChannelOutcome {
        status,
        detail: None,
        occurred_at: at,
        raw_answer: None,
        citations: vec![],
        public_url: None,
        screenshot_ref: None,
        connector_version: Some("fixture.connector.v1".into()),
        runner_evidence: proofs,
        fixture: false,
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL in GEO_TEST_DATABASE_URL"]
async fn lookup_is_scoped_fenced_append_only_and_never_rewrites_send() {
    let url =
        std::env::var("GEO_TEST_DATABASE_URL").expect("disposable test database URL required");
    let admin = sqlx::PgPool::connect(&url).await.unwrap();
    let schema = format!("lookup_test_{}", Uuid::new_v4().simple());
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

    let (scope, cycle) = create_scope(&database).await;
    let (other, other_cycle) = create_scope(&database).await;
    let channels = PgChannelJobRepository::from_database(&database);
    let lookup = PgPublicationLookupRepository::from_database(&database);
    let now = chrono::DateTime::<Utc>::from_timestamp_micros(Utc::now().timestamp_micros())
        .expect("valid timestamp");
    let target_id = Uuid::new_v4();
    let account_id = Uuid::new_v4();
    let title = "Example title";
    let body = "Example body";
    let target = ChannelTarget {
        target_id,
        input: ChannelTargetInput::Publish {
            source_id: Uuid::new_v4(),
            source_version_id: Uuid::new_v4(),
            platform: "zhihu".into(),
            account_id,
            title: title.into(),
            body: body.into(),
            body_sha256: hex::encode(Sha256::digest(body.as_bytes())),
        },
    };
    let sibling_target_id = Uuid::new_v4();
    let sibling_account_id = Uuid::new_v4();
    let mut sibling_input = target.input.clone();
    match &mut sibling_input {
        ChannelTargetInput::Publish { account_id, .. } => *account_id = sibling_account_id,
        _ => unreachable!(),
    }
    channels
        .create_plan(
            &scope,
            ChannelPlan {
                plan_id: Uuid::new_v4(),
                project_id: scope.project_id.unwrap(),
                cycle_id: cycle,
                input_hash: "lookup".into(),
                revision: 1,
                created_at: now,
                targets: vec![
                    target.clone(),
                    ChannelTarget {
                        target_id: sibling_target_id,
                        input: sibling_input,
                    },
                ],
            },
        )
        .await
        .unwrap();
    let attempt_id = Uuid::new_v4();
    channels
        .claim(&scope, target_id, attempt_id, now)
        .await
        .unwrap();
    assert_eq!(
        lookup
            .enqueue(&other, target_id, attempt_id, now)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    let preliminary = lookup
        .enqueue(&scope, target_id, attempt_id, now)
        .await
        .unwrap();
    assert_eq!(preliminary.candidate_public_url, None);
    assert_eq!(
        lookup
            .enqueue(&scope, target_id, attempt_id, now + Duration::seconds(1))
            .await
            .unwrap(),
        preliminary
    );
    let early_execution = Uuid::new_v4();
    lookup
        .claim(
            &scope,
            attempt_id,
            early_execution,
            now,
            now + Duration::minutes(1),
        )
        .await
        .unwrap();
    let early_observation = PublicationLookupObservation {
        execution_id: early_execution,
        attempt_id,
        finding: PublicationLookupFinding::Unknown,
        evidence: serde_json::json!({"reason":"no_hint"}),
        observed_at: now,
        received_at: now,
        error_code: None,
    };
    assert_eq!(
        lookup
            .finish(&scope, attempt_id, early_observation.clone(), None)
            .await
            .unwrap()
            .next_due_at,
        None
    );
    let expected = hex::encode(Sha256::digest(format!("{title}\n{body}").as_bytes()));
    let candidate_url = "https://zhuanlan.zhihu.com/p/123456";
    let original = outcome(
        ChannelOutcomeStatus::Unknown,
        now,
        vec![serde_json::json!({
            "kind": "publication_candidate",
            "schema_version": "geo.publication.candidate.v1",
            "source": "post_submit_navigation",
            "url": candidate_url,
            "expected_sha256": expected,
            "attempt_id": attempt_id,
            "target_id": target_id,
            "account_id": account_id,
            "connector_version": "fixture.connector.v1",
            "observed_at": now,
        })],
    );
    channels
        .finish(&scope, target_id, attempt_id, original.clone(), now)
        .await
        .unwrap();
    // Finalized Unknown enriches the initially empty job exactly once.
    let enriched = lookup
        .enqueue(&scope, target_id, attempt_id, now)
        .await
        .unwrap();
    assert_eq!(
        enriched.candidate_public_url.as_deref(),
        Some(candidate_url)
    );
    assert_eq!(enriched.next_due_at, Some(now));
    assert_eq!(
        lookup
            .enqueue(&scope, target_id, attempt_id, now)
            .await
            .unwrap(),
        enriched
    );
    assert_eq!(
        lookup.get(&other, attempt_id).await.unwrap_err().code,
        ErrorCode::NotFound
    );

    let second_target = Uuid::new_v4();
    let second_account = Uuid::new_v4();
    let mut second_input = target.input.clone();
    match &mut second_input {
        ChannelTargetInput::Publish { account_id, .. } => *account_id = second_account,
        _ => unreachable!(),
    }
    channels
        .create_plan(
            &other,
            ChannelPlan {
                plan_id: Uuid::new_v4(),
                project_id: other.project_id.unwrap(),
                cycle_id: other_cycle,
                input_hash: "other".into(),
                revision: 1,
                created_at: now,
                targets: vec![ChannelTarget {
                    target_id: second_target,
                    input: second_input,
                }],
            },
        )
        .await
        .unwrap();
    let second_attempt = Uuid::new_v4();
    channels
        .claim(&other, second_target, second_attempt, now)
        .await
        .unwrap();
    let second_original = outcome(ChannelOutcomeStatus::Unknown, now, vec![]);
    channels
        .finish(
            &other,
            second_target,
            second_attempt,
            second_original.clone(),
            now,
        )
        .await
        .unwrap();
    lookup
        .enqueue(&other, second_target, second_attempt, now)
        .await
        .unwrap();
    sqlx::query("UPDATE projects SET status='paused' WHERE project_id=$1")
        .bind(other.project_id.unwrap().as_uuid())
        .execute(database.pool())
        .await
        .unwrap();
    let candidates = lookup.scan_due(None, now, 1).await.unwrap();
    assert_eq!(candidates.len(), 1);
    let rest = lookup
        .scan_due(Some(candidates[0].attempt_id), now, 1)
        .await
        .unwrap();
    assert_eq!(rest.len(), 1);
    assert_eq!(
        [candidates[0].attempt_id, rest[0].attempt_id]
            .into_iter()
            .collect::<std::collections::HashSet<_>>(),
        [attempt_id, second_attempt].into_iter().collect()
    );
    assert_eq!(
        lookup.scan_due(None, now, 0).await.unwrap_err().code,
        ErrorCode::InvalidRequest
    );

    let first_execution = Uuid::new_v4();
    let collision = Uuid::new_v4();
    let (first, other_claim) = tokio::join!(
        lookup.claim(
            &scope,
            attempt_id,
            first_execution,
            now,
            now + Duration::minutes(1)
        ),
        lookup.claim(
            &scope,
            attempt_id,
            collision,
            now,
            now + Duration::minutes(1)
        ),
    );
    assert_eq!(first.is_ok() as u8 + other_claim.is_ok() as u8, 1);
    let first_execution = if first.is_ok() {
        first_execution
    } else {
        collision
    };
    assert_eq!(lookup.get(&scope, attempt_id).await.unwrap().query_count, 2);
    // The test advances lease state directly; no wall-clock sleep or fabricated
    // client timestamp can substitute for the repository's database clock.
    sqlx::query("UPDATE publication_lookup_jobs SET lease_expires_at=$2 WHERE attempt_id=$1")
        .bind(attempt_id)
        .bind(now + Duration::microseconds(1))
        .execute(database.pool())
        .await
        .unwrap();
    let expired_observation = PublicationLookupObservation {
        execution_id: first_execution,
        attempt_id,
        finding: PublicationLookupFinding::Unknown,
        evidence: serde_json::json!({"reason":"no_asset"}),
        observed_at: now,
        received_at: now,
        error_code: None,
    };
    assert_eq!(
        lookup
            .finish(
                &scope,
                attempt_id,
                expired_observation,
                Some(now + Duration::minutes(2))
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let later = chrono::DateTime::<Utc>::from_timestamp_micros(Utc::now().timestamp_micros())
        .expect("valid timestamp");
    assert_eq!(
        lookup
            .claim(
                &scope,
                attempt_id,
                first_execution,
                later,
                later + Duration::minutes(1)
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let recovered = Uuid::new_v4();
    lookup
        .claim(
            &scope,
            attempt_id,
            recovered,
            later,
            later + Duration::minutes(1),
        )
        .await
        .unwrap();
    let stale = PublicationLookupObservation {
        execution_id: first_execution,
        attempt_id,
        finding: PublicationLookupFinding::Unknown,
        evidence: serde_json::json!({"reason":"no_asset"}),
        observed_at: later,
        received_at: later,
        error_code: None,
    };
    assert_eq!(
        lookup
            .finish(
                &scope,
                attempt_id,
                stale,
                Some(later + Duration::seconds(20))
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let observation = PublicationLookupObservation {
        execution_id: recovered,
        attempt_id,
        finding: PublicationLookupFinding::AssetObserved,
        evidence: serde_json::json!({"url":candidate_url,"send_association":false}),
        observed_at: later,
        received_at: later,
        error_code: None,
    };
    let due = later + Duration::seconds(20);
    let completed = lookup
        .finish(&scope, attempt_id, observation.clone(), Some(due))
        .await
        .unwrap();
    assert_eq!(completed.next_due_at, Some(due));
    assert_eq!(completed.query_count, 3);
    assert_eq!(
        lookup
            .finish(&scope, attempt_id, observation.clone(), Some(due))
            .await
            .unwrap(),
        completed
    );
    assert_eq!(
        lookup
            .finish(&scope, attempt_id, observation.clone(), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        lookup.observations(&scope, attempt_id).await.unwrap(),
        vec![early_observation, observation]
    );
    let report_initial = lookup
        .report_asset_observations(&scope, &[target_id, sibling_target_id], later)
        .await
        .unwrap();
    assert_eq!(report_initial.len(), 1);
    assert_eq!(report_initial[0].job.attempt_id, attempt_id);
    assert_eq!(report_initial[0].original_target_id, target_id);
    assert_eq!(report_initial[0].observation.execution_id, recovered);
    assert!(
        lookup
            .report_asset_observations(&other, &[target_id], later)
            .await
            .unwrap()
            .is_empty()
    );
    // More than a UI page of identical-receipt observations must be read in
    // deterministic order and bounded to the latest 32 per original send.
    // Malformed newest evidence can be skipped by the API validator.
    let same_time = later + Duration::seconds(2);
    let mut execution_ids = Vec::new();
    for _ in 0..25 {
        let execution_id = Uuid::new_v4();
        execution_ids.push(execution_id);
        sqlx::query(
            "INSERT INTO publication_lookup_executions \
             (execution_id,operator_id,tenant_id,project_id,attempt_id,claimed_at,expires_at) \
             VALUES($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(execution_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(scope.project_id.unwrap().as_uuid())
        .bind(attempt_id)
        .bind(same_time)
        .bind(same_time + Duration::seconds(5))
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO publication_lookup_observations \
             (execution_id,operator_id,tenant_id,project_id,attempt_id,finding,evidence,\
              observed_at,received_at) VALUES($1,$2,$3,$4,$5,'asset_observed',$6,$7,$7)",
        )
        .bind(execution_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(scope.project_id.unwrap().as_uuid())
        .bind(attempt_id)
        .bind(serde_json::json!({"private_internal":"never returned by report"}))
        .bind(same_time)
        .execute(database.pool())
        .await
        .unwrap();
    }
    let latest = lookup
        .report_asset_observations(&scope, &[target_id, target_id], same_time)
        .await
        .unwrap();
    assert_eq!(latest.len(), 26);
    assert_eq!(
        latest[0].observation.execution_id,
        *execution_ids.iter().max().unwrap()
    );
    assert_eq!(latest.last().unwrap().observation.execution_id, recovered);
    for _ in 0..10 {
        let execution_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO publication_lookup_executions \
             (execution_id,operator_id,tenant_id,project_id,attempt_id,claimed_at,expires_at) \
             VALUES($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(execution_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(scope.project_id.unwrap().as_uuid())
        .bind(attempt_id)
        .bind(same_time)
        .bind(same_time + Duration::seconds(5))
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO publication_lookup_observations \
             (execution_id,operator_id,tenant_id,project_id,attempt_id,finding,evidence,\
              observed_at,received_at) VALUES($1,$2,$3,$4,$5,'asset_observed',$6,$7,$7)",
        )
        .bind(execution_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(scope.project_id.unwrap().as_uuid())
        .bind(attempt_id)
        .bind(serde_json::json!({"private_internal":"never returned by report"}))
        .bind(same_time)
        .execute(database.pool())
        .await
        .unwrap();
    }
    let bounded = lookup
        .report_asset_observations(&scope, &[target_id], same_time)
        .await
        .unwrap();
    assert_eq!(bounded.len(), 32);
    assert!(
        bounded
            .iter()
            .all(|row| row.observation.received_at == same_time)
    );
    let cutoff = lookup
        .report_asset_observations(&scope, &[target_id], later)
        .await
        .unwrap();
    assert_eq!(cutoff.len(), 1);
    assert_eq!(cutoff[0].observation.execution_id, recovered);
    assert!(
        lookup
            .report_asset_observations(&scope, &[second_target], same_time)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        channels
            .get_target(&scope, target_id)
            .await
            .unwrap()
            .attempts[0]
            .outcome,
        Some(original)
    );
    let original_json: serde_json::Value =
        sqlx::query_scalar("SELECT outcome FROM channel_execution_attempts WHERE attempt_id=$1")
            .bind(attempt_id)
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert_eq!(original_json["status"], "unknown");
    assert_eq!(
        lookup
            .claim(
                &other,
                attempt_id,
                Uuid::new_v4(),
                due,
                due + Duration::seconds(5)
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    let hinted_target = Uuid::new_v4();
    let hinted_attempt = Uuid::new_v4();
    let hinted_account = Uuid::new_v4();
    let mut hinted_input = target.input.clone();
    match &mut hinted_input {
        ChannelTargetInput::Publish { account_id, .. } => *account_id = hinted_account,
        _ => unreachable!(),
    }
    let (third_scope, third_cycle) = create_scope(&database).await;
    channels
        .create_plan(
            &third_scope,
            ChannelPlan {
                plan_id: Uuid::new_v4(),
                project_id: third_scope.project_id.unwrap(),
                cycle_id: third_cycle,
                input_hash: "hinted".into(),
                revision: 1,
                created_at: now,
                targets: vec![ChannelTarget {
                    target_id: hinted_target,
                    input: hinted_input,
                }],
            },
        )
        .await
        .unwrap();
    channels
        .claim(&third_scope, hinted_target, hinted_attempt, now)
        .await
        .unwrap();
    let hinted_outcome = outcome(
        ChannelOutcomeStatus::Unknown,
        now,
        vec![serde_json::json!({
            "kind": "publication_candidate",
            "schema_version": "geo.publication.candidate.v1",
            "source": "post_submit_navigation",
            "url": candidate_url,
            "expected_sha256": hex::encode(Sha256::digest(format!("{title}\n{body}").as_bytes())),
            "attempt_id": hinted_attempt,
            "target_id": hinted_target,
            "account_id": hinted_account,
            "connector_version": "fixture.connector.v1",
            "observed_at": now,
        })],
    );
    channels
        .finish(
            &third_scope,
            hinted_target,
            hinted_attempt,
            hinted_outcome.clone(),
            now,
        )
        .await
        .unwrap();
    let hinted = lookup
        .enqueue(&third_scope, hinted_target, hinted_attempt, now)
        .await
        .unwrap();
    assert_eq!(hinted.candidate_public_url.as_deref(), Some(candidate_url));
    assert_eq!(
        lookup
            .enqueue(&third_scope, hinted_target, hinted_attempt, now)
            .await
            .unwrap(),
        hinted
    );
    assert_eq!(
        channels
            .get_target(&third_scope, hinted_target)
            .await
            .unwrap()
            .attempts[0]
            .outcome,
        Some(hinted_outcome)
    );
    // A preexisting different hint is never overwritten by enqueue replay.
    sqlx::query(
        "UPDATE publication_lookup_jobs SET candidate_public_url='https://www.zhihu.com/p/999' \
         WHERE attempt_id=$1",
    )
    .bind(hinted_attempt)
    .execute(database.pool())
    .await
    .unwrap();
    assert_eq!(
        lookup
            .enqueue(&third_scope, hinted_target, hinted_attempt, now)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let sibling_attempt = Uuid::new_v4();
    channels
        .claim(&scope, sibling_target_id, sibling_attempt, now)
        .await
        .unwrap();
    channels
        .finish(
            &scope,
            sibling_target_id,
            sibling_attempt,
            outcome(ChannelOutcomeStatus::Unknown, now, vec![]),
            now,
        )
        .await
        .unwrap();
    lookup
        .enqueue(&scope, sibling_target_id, sibling_attempt, now)
        .await
        .unwrap();
    // Explicit read pagination regression: >20 rows, identical received_at
    // ties, cursor isolation and a genuine bounded SQL page.
    // Keep the pagination batch newer than the report-candidate batch above.
    // Both batches remain in the append-only history and must be returned.
    let tied_at = later + Duration::seconds(3);
    for number in 1..=25_u128 {
        let execution = Uuid::from_u128(number);
        sqlx::query(
            "INSERT INTO publication_lookup_executions \
             (execution_id,operator_id,tenant_id,project_id,attempt_id,claimed_at,expires_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(execution)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(scope.project_id.unwrap().as_uuid())
        .bind(attempt_id)
        .bind(now)
        .bind(now + Duration::minutes(5))
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO publication_lookup_observations \
             (execution_id,operator_id,tenant_id,project_id,attempt_id,finding,evidence,observed_at,received_at) \
             VALUES ($1,$2,$3,$4,$5,'unknown','{}'::jsonb,$6,$6)",
        )
        .bind(execution)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(scope.project_id.unwrap().as_uuid())
        .bind(attempt_id)
        .bind(tied_at)
        .execute(database.pool())
        .await
        .unwrap();
    }
    let page = lookup
        .observation_page(&scope, attempt_id, None, 20)
        .await
        .unwrap();
    assert_eq!(page.len(), 21);
    assert_eq!(page[0].execution_id, Uuid::from_u128(25));
    assert_eq!(page[19].execution_id, Uuid::from_u128(6));
    let second = lookup
        .observation_page(&scope, attempt_id, Some(page[19].execution_id), 20)
        .await
        .unwrap();
    assert_eq!(second.len(), 21);
    assert_eq!(second[0].execution_id, Uuid::from_u128(5));
    let mut all_paged = page[..20].to_vec();
    let mut current_page = second;
    loop {
        assert!(current_page.len() <= 21);
        let has_more = current_page.len() > 20;
        current_page.truncate(20);
        let cursor = current_page.last().map(|item| item.execution_id);
        all_paged.extend(current_page);
        if !has_more {
            break;
        }
        current_page = lookup
            .observation_page(&scope, attempt_id, cursor, 20)
            .await
            .unwrap();
    }
    let mut expected_history = lookup.observations(&scope, attempt_id).await.unwrap();
    expected_history.sort_by_key(|item| std::cmp::Reverse((item.received_at, item.execution_id)));
    assert_eq!(expected_history.len(), 62);
    assert_eq!(all_paged, expected_history);
    assert_eq!(
        lookup
            .observation_page(&other, attempt_id, Some(page[19].execution_id), 20)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );
    assert_eq!(
        lookup
            .observation_page(&scope, sibling_attempt, Some(page[19].execution_id), 20)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );
    assert_eq!(
        lookup
            .observation_page(&scope, attempt_id, Some(Uuid::new_v4()), 20)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );
    let wrong_association = sqlx::query(
        "INSERT INTO publication_lookup_observations \
         (execution_id,operator_id,tenant_id,project_id,attempt_id,finding,evidence,observed_at,received_at) \
         VALUES ($1,$2,$3,$4,$5,'unknown','{}'::jsonb,$6,$6)",
    )
    .bind(first_execution)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(scope.project_id.unwrap().as_uuid())
    .bind(sibling_attempt)
    .bind(now)
    .execute(database.pool())
    .await
    .unwrap_err();
    assert_eq!(
        wrong_association
            .as_database_error()
            .and_then(|db| db.code())
            .as_deref(),
        Some("23503")
    );
    // Only jobs are mutable. Execution and observation history is immutable
    // even to a direct SQL writer.
    for query in [
        "UPDATE publication_lookup_executions SET claimed_at=claimed_at WHERE execution_id=$1",
        "DELETE FROM publication_lookup_executions WHERE execution_id=$1",
        "UPDATE publication_lookup_observations SET finding=finding WHERE execution_id=$1",
        "DELETE FROM publication_lookup_observations WHERE execution_id=$1",
    ] {
        let error = sqlx::query(query)
            .bind(recovered)
            .execute(database.pool())
            .await
            .unwrap_err();
        assert_eq!(
            error
                .as_database_error()
                .and_then(|db| db.code())
                .as_deref(),
            Some("P0001")
        );
    }
    let check_error = sqlx::query(
        "INSERT INTO publication_lookup_executions \
         (execution_id,operator_id,tenant_id,project_id,attempt_id,claimed_at,expires_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$6)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(scope.project_id.unwrap().as_uuid())
    .bind(attempt_id)
    .bind(now)
    .execute(database.pool())
    .await
    .unwrap_err();
    assert_eq!(
        check_error
            .as_database_error()
            .and_then(|db| db.code())
            .as_deref(),
        Some("23514")
    );
    let invalid_times = sqlx::query(
        "INSERT INTO publication_lookup_observations \
         (execution_id,operator_id,tenant_id,project_id,attempt_id,finding,evidence,observed_at,received_at) \
         VALUES ($1,$2,$3,$4,$5,'unknown','{}'::jsonb,$6,$7)",
    )
    .bind(first_execution)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(scope.project_id.unwrap().as_uuid())
    .bind(attempt_id)
    .bind(now + Duration::minutes(2))
    .bind(now)
    .execute(database.pool())
    .await
    .unwrap_err();
    assert_eq!(
        invalid_times
            .as_database_error()
            .and_then(|db| db.code())
            .as_deref(),
        Some("23514")
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL in GEO_TEST_DATABASE_URL"]
async fn discovery_paginates_ambiguous_sends_and_enriches_only_newly_finalized_jobs() {
    let url =
        std::env::var("GEO_TEST_DATABASE_URL").expect("disposable test database URL required");
    let admin = sqlx::PgPool::connect(&url).await.unwrap();
    let schema = format!("lookup_discovery_{}", Uuid::new_v4().simple());
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
    let channels = PgChannelJobRepository::from_database(&database);
    let lookup = PgPublicationLookupRepository::from_database(&database);
    let (scope, cycle) = create_scope(&database).await;
    let (paused, paused_cycle) = create_scope(&database).await;
    let now = chrono::DateTime::<Utc>::from_timestamp_micros(Utc::now().timestamp_micros())
        .expect("valid timestamp");
    let old = now - Duration::minutes(6);
    let near = now - Duration::minutes(4);
    let publication = || ChannelTarget {
        target_id: Uuid::new_v4(),
        input: ChannelTargetInput::Publish {
            source_id: Uuid::new_v4(),
            source_version_id: Uuid::new_v4(),
            platform: "zhihu".into(),
            account_id: Uuid::new_v4(),
            title: "Example title".into(),
            body: "Example body".into(),
            body_sha256: hex::encode(Sha256::digest(b"Example body")),
        },
    };
    let stale = publication();
    let fresh = publication();
    let missing_unknown = publication();
    let late_hint = publication();
    let no_version = publication();
    let failed = publication();
    let verified_fixture = publication();
    let measure = ChannelTarget {
        target_id: Uuid::new_v4(),
        input: ChannelTargetInput::Measure {
            account_id: Uuid::new_v4(),
            provider: "test".into(),
            model: "test".into(),
            surface: "web".into(),
            search_mode: "web_search".into(),
            protocol_version: "v1".into(),
            question_set_version: "v1".into(),
            question: "Example".into(),
            market: "global".into(),
            language: "en".into(),
            scheduled_at: old,
            sample_ordinal: 1,
            question_binding: None,
        },
    };
    let targets = [
        stale.clone(),
        fresh.clone(),
        missing_unknown.clone(),
        late_hint.clone(),
        no_version.clone(),
        failed.clone(),
        verified_fixture.clone(),
        measure.clone(),
    ];
    channels
        .create_plan(
            &scope,
            ChannelPlan {
                plan_id: Uuid::new_v4(),
                project_id: scope.project_id.unwrap(),
                cycle_id: cycle,
                input_hash: "discovery".into(),
                revision: 1,
                created_at: old,
                targets: targets.to_vec(),
            },
        )
        .await
        .unwrap();
    let paused_target = publication();
    channels
        .create_plan(
            &paused,
            ChannelPlan {
                plan_id: Uuid::new_v4(),
                project_id: paused.project_id.unwrap(),
                cycle_id: paused_cycle,
                input_hash: "paused-discovery".into(),
                revision: 1,
                created_at: old,
                targets: vec![paused_target.clone()],
            },
        )
        .await
        .unwrap();
    let mut attempts = std::collections::HashMap::new();
    for target in &targets {
        let attempt = Uuid::new_v4();
        let claimed_at = if target.target_id == fresh.target_id {
            near
        } else {
            old
        };
        channels
            .claim(&scope, target.target_id, attempt, claimed_at)
            .await
            .unwrap();
        attempts.insert(target.target_id, attempt);
    }
    let paused_attempt = Uuid::new_v4();
    channels
        .claim(&paused, paused_target.target_id, paused_attempt, old)
        .await
        .unwrap();
    let id = |target: &ChannelTarget| attempts[&target.target_id];

    let early = lookup
        .enqueue(&scope, late_hint.target_id, id(&late_hint), old)
        .await
        .unwrap();
    assert!(early.connector_version.is_none());
    assert!(early.candidate_public_url.is_none());
    let mut unknown_without_version = outcome(ChannelOutcomeStatus::Unknown, now, vec![]);
    unknown_without_version.connector_version = None;
    channels
        .finish(
            &scope,
            no_version.target_id,
            id(&no_version),
            unknown_without_version,
            now,
        )
        .await
        .unwrap();
    channels
        .finish(
            &scope,
            missing_unknown.target_id,
            id(&missing_unknown),
            outcome(ChannelOutcomeStatus::Unknown, now, vec![]),
            now,
        )
        .await
        .unwrap();
    channels
        .finish(
            &scope,
            failed.target_id,
            id(&failed),
            outcome(ChannelOutcomeStatus::Failed, now, vec![]),
            now,
        )
        .await
        .unwrap();
    let mut fixture_success = outcome(ChannelOutcomeStatus::Verified, now, vec![]);
    fixture_success.fixture = true;
    channels
        .finish(
            &scope,
            verified_fixture.target_id,
            id(&verified_fixture),
            fixture_success,
            now,
        )
        .await
        .unwrap();
    channels
        .finish(
            &scope,
            measure.target_id,
            id(&measure),
            outcome(ChannelOutcomeStatus::Unknown, now, vec![]),
            now,
        )
        .await
        .unwrap();
    let mut fixture_unknown = outcome(ChannelOutcomeStatus::Unknown, now, vec![]);
    fixture_unknown.fixture = true;
    channels
        .finish(
            &paused,
            paused_target.target_id,
            paused_attempt,
            fixture_unknown,
            now,
        )
        .await
        .unwrap();
    sqlx::query("UPDATE projects SET status='paused' WHERE project_id=$1")
        .bind(paused.project_id.unwrap().as_uuid())
        .execute(database.pool())
        .await
        .unwrap();
    // Before the bounded send window expires, even a finalized Unknown
    // cannot produce a job; a finished early job has no new data yet.
    let early_page = lookup
        .scan_unresolved(
            None,
            old + Duration::minutes(5) - Duration::microseconds(1),
            100,
        )
        .await
        .unwrap();
    assert!(early_page.is_empty());

    let mut pages = vec![];
    let mut cursor = None;
    loop {
        let page = lookup.scan_unresolved(cursor, now, 2).await.unwrap();
        if page.is_empty() {
            break;
        }
        assert!(page.len() <= 2);
        cursor = page.last().map(|item| item.attempt_id);
        pages.extend(page);
    }
    let expected = [
        id(&stale),
        id(&missing_unknown),
        id(&no_version),
        paused_attempt,
    ];
    assert_eq!(pages.len(), expected.len());
    assert_eq!(
        pages
            .iter()
            .map(|item| item.attempt_id)
            .collect::<std::collections::HashSet<_>>(),
        expected.into_iter().collect()
    );
    assert!(
        pages
            .windows(2)
            .all(|pair| pair[0].attempt_id < pair[1].attempt_id)
    );
    assert!(pages.iter().any(|item| item.scope == paused
        && item.target_id == paused_target.target_id
        && item.attempt_id == paused_attempt));
    for candidate in &pages {
        lookup
            .enqueue(
                &candidate.scope,
                candidate.target_id,
                candidate.attempt_id,
                now,
            )
            .await
            .unwrap();
    }
    assert!(
        lookup
            .scan_unresolved(None, now, 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        lookup
            .get(&paused, paused_attempt)
            .await
            .unwrap()
            .candidate_public_url,
        None
    );

    let candidate_url = "https://www.zhihu.com/p/12345";
    channels
        .finish(
            &scope,
            late_hint.target_id,
            id(&late_hint),
            outcome(
                ChannelOutcomeStatus::Unknown,
                now,
                vec![serde_json::json!({
                    "kind": "publication_candidate",
                    "schema_version": "geo.publication.candidate.v1",
                    "source": "post_submit_navigation",
                    "url": candidate_url,
                    "expected_sha256": hex::encode(Sha256::digest(b"Example title\nExample body")),
                    "attempt_id": id(&late_hint),
                    "target_id": late_hint.target_id,
                    "account_id": late_hint.input.account_id(),
                    "connector_version": "fixture.connector.v1",
                    "observed_at": now,
                })],
            ),
            now,
        )
        .await
        .unwrap();
    let revisit = lookup.scan_unresolved(None, now, 100).await.unwrap();
    assert_eq!(revisit.len(), 1);
    assert_eq!(revisit[0].attempt_id, id(&late_hint));
    let enriched = lookup
        .enqueue(
            &revisit[0].scope,
            revisit[0].target_id,
            revisit[0].attempt_id,
            now,
        )
        .await
        .unwrap();
    assert_eq!(
        enriched.candidate_public_url.as_deref(),
        Some(candidate_url)
    );
    assert!(
        lookup
            .scan_unresolved(None, now, 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        lookup.scan_unresolved(None, now, 0).await.unwrap_err().code,
        ErrorCode::InvalidRequest
    );
    assert_eq!(
        lookup
            .scan_unresolved(None, now, 1001)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );

    let fresh_due = lookup
        .scan_unresolved(None, near + Duration::minutes(5), 100)
        .await
        .unwrap();
    assert_eq!(fresh_due.len(), 1);
    assert_eq!(fresh_due[0].attempt_id, id(&fresh));
    assert_eq!(
        lookup
            .enqueue(
                &fresh_due[0].scope,
                fresh_due[0].target_id,
                fresh_due[0].attempt_id,
                now
            )
            .await
            .unwrap()
            .candidate_public_url,
        None
    );
    assert_eq!(
        lookup
            .scan_unresolved(None, near + Duration::minutes(5), 100)
            .await
            .unwrap(),
        vec![]
    );
}
