//! Requires a disposable database; no external account or measurement is used.
use chrono::Utc;
use geo_domain::{
    ChannelJobRepository, ChannelTarget, ChannelTargetInput, FrozenQuestionBinding,
    QuestionPurpose, QuestionReference, StandaloneMeasurementPlan, TenantScope,
};
use geo_persistence::{Database, DatabaseConfig, PgChannelJobRepository};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn standalone_measurement_draft_concurrency_restart_and_owner_constraints() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database URL");
    let database = Database::connect_and_migrate(&DatabaseConfig::from_url(url).unwrap())
        .await
        .unwrap();
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    let project = Uuid::new_v4();
    sqlx::query("INSERT INTO operators(operator_id,slug,display_name) VALUES($1,$2,'Test')")
        .bind(operator)
        .bind(format!("measurement-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants(tenant_id,operator_id,slug,display_name) VALUES($1,$2,$3,'Test')",
    )
    .bind(tenant)
    .bind(operator)
    .bind(format!("measurement-{tenant}"))
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO projects(project_id,operator_id,tenant_id,slug,display_name) VALUES($1,$2,$3,$4,'Test')")
        .bind(project).bind(operator).bind(tenant).bind(format!("measurement-{project}")).execute(database.pool()).await.unwrap();
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.into()));
    let now = Utc::now();
    let plan = StandaloneMeasurementPlan {
        plan_id: Uuid::new_v4(),
        project_id: project.into(),
        title: "General topic".into(),
        input_hash: "frozen".into(),
        revision: 1,
        created_at: now,
        targets: vec![ChannelTarget {
            target_id: Uuid::new_v4(),
            input: ChannelTargetInput::Measure {
                account_id: Uuid::new_v4(),
                provider: "fixture".into(),
                model: "fixed".into(),
                surface: "consumer_web".into(),
                search_mode: "web_search".into(),
                protocol_version: "v1".into(),
                question_set_version: "ad-hoc".into(),
                question: "How are eclipses predicted?".into(),
                market: "global".into(),
                language: "en".into(),
                scheduled_at: now,
                sample_ordinal: 0,
                question_binding: None,
            },
        }],
    };
    let repo = PgChannelJobRepository::from_database(&database);
    let mut concurrent = plan.clone();
    concurrent.plan_id = Uuid::new_v4();
    concurrent.targets[0].target_id = Uuid::new_v4();
    let (a, b) = tokio::join!(
        repo.create_measurement_plan(&scope, "key", "request", plan),
        repo.create_measurement_plan(&scope, "key", "request", concurrent)
    );
    let saved = a.unwrap();
    assert_eq!(saved, b.unwrap());
    assert!(
        repo.create_measurement_plan(&scope, "key", "different", saved.clone())
            .await
            .is_err()
    );
    let restarted = PgChannelJobRepository::from_database(&database);
    assert_eq!(
        restarted
            .replay_measurement_plan(&scope, "key", "request")
            .await
            .unwrap(),
        Some(saved.clone())
    );
    assert_eq!(
        restarted
            .list_measurement_plans(&scope, None, 10)
            .await
            .unwrap(),
        vec![saved.clone()]
    );
    assert!(
        restarted
            .list_measurement_plans(&scope, Some(saved.plan_id), 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        restarted
            .list_optimization_measurement_plans(&scope, None, 2)
            .await
            .unwrap()
            .is_empty()
    );
    let base = Uuid::new_v4().as_u128() & !0xff;
    let version = Uuid::new_v4();
    let mut eligible_ids = Vec::new();
    for (offset, purpose) in [
        (1, Some(QuestionPurpose::FrozenEvaluation)),
        (2, None),
        (3, Some(QuestionPurpose::Optimization)),
        (4, Some(QuestionPurpose::FrozenEvaluation)),
        (5, None),
        (6, Some(QuestionPurpose::Optimization)),
    ] {
        let mut candidate = saved.clone();
        candidate.plan_id = Uuid::from_u128(base + offset);
        candidate.input_hash = candidate.plan_id.to_string();
        candidate.targets[0].target_id = Uuid::new_v4();
        if let ChannelTargetInput::Measure {
            question_set_version,
            question_binding,
            ..
        } = &mut candidate.targets[0].input
        {
            *question_set_version = version.to_string();
            *question_binding = purpose.map(|purpose| FrozenQuestionBinding {
                reference: QuestionReference {
                    question_set_id: Uuid::new_v4(),
                    question_set_version_id: version,
                    question_id: Uuid::new_v4(),
                    question_revision_id: Uuid::new_v4(),
                },
                purpose,
                split_policy_version: "synthetic_v1".into(),
            });
        }
        if purpose == Some(QuestionPurpose::Optimization) {
            eligible_ids.push(candidate.plan_id);
        }
        restarted
            .create_measurement_plan(
                &scope,
                &candidate.plan_id.to_string(),
                &candidate.plan_id.to_string(),
                candidate,
            )
            .await
            .unwrap();
    }
    assert_eq!(
        restarted
            .list_optimization_measurement_plans(&scope, Some(Uuid::from_u128(base)), 1)
            .await
            .unwrap()
            .into_iter()
            .map(|plan| plan.plan_id)
            .collect::<Vec<_>>(),
        eligible_ids[..1]
    );
    assert_eq!(
        restarted
            .list_optimization_measurement_plans(&scope, Some(eligible_ids[0]), 1)
            .await
            .unwrap()
            .into_iter()
            .map(|plan| plan.plan_id)
            .collect::<Vec<_>>(),
        eligible_ids[1..]
    );
    let other = TenantScope::new(
        scope.operator_id,
        scope.tenant_id,
        Some(Uuid::new_v4().into()),
    );
    assert!(
        restarted
            .get_measurement_plan(&other, saved.plan_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        restarted
            .list_optimization_measurement_plans(&other, None, 2)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        restarted
            .cycle_inputs(&scope, saved.plan_id, now)
            .await
            .unwrap()
            .measurements
            .is_none()
    );
    let cycle_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM optimization_cycles WHERE project_id=$1")
            .bind(project)
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert_eq!(cycle_count, 0);
    let target_id = saved.targets[0].target_id;
    for invalid_update in [
        "UPDATE channel_execution_targets SET cycle_id=$2 WHERE target_id=$1",
        "UPDATE channel_execution_targets SET plan_id=$2 WHERE target_id=$1",
        "UPDATE channel_execution_targets SET publication_intent_id=$2 WHERE target_id=$1",
    ] {
        assert!(
            sqlx::query(invalid_update)
                .bind(target_id)
                .bind(Uuid::new_v4())
                .execute(database.pool())
                .await
                .is_err()
        );
    }
    for invalid_update in [
        "UPDATE channel_execution_targets SET ordinal=NULL WHERE target_id=$1",
        "UPDATE channel_execution_targets SET kind='publish' WHERE target_id=$1",
        "UPDATE channel_execution_targets SET measurement_plan_id=NULL WHERE target_id=$1",
    ] {
        assert!(
            sqlx::query(invalid_update)
                .bind(target_id)
                .execute(database.pool())
                .await
                .is_err()
        );
    }
    let reservation = Uuid::new_v4();
    restarted
        .reserve_account(
            &scope,
            saved.targets[0].input.account_id(),
            reservation,
            now,
            now + chrono::Duration::seconds(30),
        )
        .await
        .unwrap();
    let attempt = Uuid::new_v4();
    restarted
        .claim_reserved(&scope, target_id, attempt, reservation, now)
        .await
        .unwrap();
    assert_eq!(
        PgChannelJobRepository::from_database(&database)
            .get_target(&scope, target_id)
            .await
            .unwrap()
            .attempts[0]
            .attempt_id,
        attempt
    );
}
