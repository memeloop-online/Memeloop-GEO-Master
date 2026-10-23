use chrono::{DateTime, Duration, Utc};
use geo_domain::{
    EffectiveObservation, ErrorCode, MeasurementPeriodSample, MeasurementPeriodWindow,
    ObservationAnalysisSource, ReportRepository, SavedAnalysisProvenance, TenantScope,
    freeze_measurement_period, preview_measurement_period, preview_measurement_period_at_cutoff,
};
use geo_persistence::{Database, DatabaseConfig, PgReportRepository};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn cycle_free_reports_freeze_concurrent_create_correction_and_restart() {
    let config = DatabaseConfig::from_url(
        std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database required"),
    )
    .unwrap();
    let database = Database::connect_and_migrate(&config).await.unwrap();
    let (operator, tenant, project, other_project) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'Synthetic')")
        .bind(operator)
        .bind(format!("period-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,'Synthetic')")
        .bind(tenant).bind(operator).bind(format!("period-{tenant}")).execute(database.pool()).await.unwrap();
    for id in [project, other_project] {
        sqlx::query("INSERT INTO projects (project_id,operator_id,tenant_id,slug,display_name) VALUES ($1,$2,$3,$4,'Synthetic')")
            .bind(id).bind(operator).bind(tenant).bind(format!("period-{id}")).execute(database.pool()).await.unwrap();
    }
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.into()));
    let other = TenantScope::new(operator.into(), tenant.into(), Some(other_project.into()));
    let now = DateTime::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap();
    let window = MeasurementPeriodWindow {
        start_at: now - Duration::days(7),
        end_at: now,
        report_timezone: "UTC".into(),
    };
    let sample = MeasurementPeriodSample {
        plan_id: Uuid::new_v4(),
        target_id: Uuid::new_v4(),
        attempt_id: Some(Uuid::new_v4()),
        comparison_key: "provider|model|consumer_web|web_search".into(),
        conditions: geo_domain::MeasurementReportConditions {
            question: "Synthetic question".into(),
            provider: "provider".into(),
            model: "model".into(),
            surface: "consumer_web".into(),
            search_mode: "web_search".into(),
            protocol_version: "v1".into(),
            question_set_version: "synthetic.v1".into(),
            market: "generic".into(),
            language: "en".into(),
            sample_ordinal: 0,
        },
        recommendations: None,
        question_binding: None,
        scheduled_at: now - Duration::hours(1),
        original_status: "unknown".into(),
        observed_live: false,
        observation: None,
    };
    let repository = PgReportRepository::from_database(&database);
    let proposed = freeze_measurement_period(
        &scope,
        preview_measurement_period(&scope, &window, None, vec![sample.clone()], now).unwrap(),
        1,
        None,
    )
    .unwrap();
    let later = freeze_measurement_period(
        &scope,
        preview_measurement_period(
            &scope,
            &window,
            None,
            vec![sample.clone()],
            now + Duration::seconds(1),
        )
        .unwrap(),
        1,
        None,
    )
    .unwrap();
    let (first, replay) = tokio::join!(
        repository.create_measurement_period(&scope, proposed),
        repository.create_measurement_period(&scope, later),
    );
    let first = first.unwrap();
    assert_eq!(first, replay.unwrap());
    let analyzed = now + Duration::minutes(1);
    let mut interpreted = sample.clone();
    interpreted.observation = Some(EffectiveObservation {
        raw_answer: "Saved answer".into(),
        citations: vec!["https://example.test/source".into()],
        observed_at: sample.scheduled_at,
        received_at: sample.scheduled_at + Duration::seconds(1),
        provenance: Some(SavedAnalysisProvenance {
            revision_id: Uuid::new_v4(),
            source: ObservationAnalysisSource::AttemptEvidence { evidence_index: 0 },
            source_sha256: "a".repeat(64),
            observed_at: sample.scheduled_at,
            analyzed_at: analyzed,
            actual_model: "parser-model".into(),
            config_revision: Some(1),
            prompt_version: "v1".into(),
            parser_version: "v1".into(),
        }),
    });
    interpreted.recommendations = Some(serde_json::json!({
        "version":"geo.recommendations.v1","status":"verified",
        "items":[{"name":"Synthetic product","mention_order":1,"rank":1,"citation_urls":["https://example.test/source"]}]
    }));
    let proposed = freeze_measurement_period(
        &scope,
        preview_measurement_period(&scope, &window, None, vec![interpreted.clone()], analyzed)
            .unwrap(),
        2,
        Some(first.report_id),
    )
    .unwrap();
    let (correction, replay) = tokio::join!(
        repository.create_measurement_period(&scope, proposed.clone()),
        repository.create_measurement_period(&scope, proposed),
    );
    let correction = correction.unwrap();
    assert_eq!(correction, replay.unwrap());
    assert_eq!(correction.coverage.planned, 1);
    assert_eq!(correction.coverage.counts["unknown"], 1);
    assert_eq!(correction.coverage.grounded_saved_analysis, 1);
    assert_eq!(
        correction.samples[0].recommendations,
        interpreted.recommendations
    );
    let restarted = PgReportRepository::from_database(&database);
    assert_eq!(
        restarted
            .get_measurement_period(&scope, first.report_id)
            .await
            .unwrap(),
        first
    );
    assert_eq!(
        restarted
            .get_measurement_period(&scope, correction.report_id)
            .await
            .unwrap(),
        correction
    );
    assert_eq!(
        restarted
            .list_measurement_periods(&scope, None)
            .await
            .unwrap()
            .len(),
        2
    );
    // A distinct delayed report keeps the cutoff independent of generation,
    // even though the late interpretation is already present when assembled.
    let delayed_window = MeasurementPeriodWindow {
        report_timezone: "Asia/Shanghai".into(),
        ..window
    };
    let generated = analyzed + Duration::hours(1);
    let delayed = freeze_measurement_period(
        &scope,
        preview_measurement_period_at_cutoff(
            &scope,
            &delayed_window,
            None,
            vec![interpreted.clone()],
            None,
            now,
            generated,
        )
        .unwrap(),
        1,
        None,
    )
    .unwrap();
    let delayed = restarted
        .create_measurement_period(&scope, delayed)
        .await
        .unwrap();
    assert_eq!(delayed.evidence_as_of, now);
    assert_eq!(delayed.generated_at, generated);
    assert_eq!(delayed.coverage.planned, 1);
    assert_eq!(delayed.coverage.grounded_saved_analysis, 0);
    assert!(delayed.samples[0].observation.is_none());
    assert!(delayed.samples[0].recommendations.is_none());
    let corrected = freeze_measurement_period(
        &scope,
        preview_measurement_period_at_cutoff(
            &scope,
            &delayed_window,
            None,
            vec![interpreted],
            None,
            analyzed,
            generated + Duration::minutes(1),
        )
        .unwrap(),
        2,
        Some(delayed.report_id),
    )
    .unwrap();
    let corrected = restarted
        .create_measurement_period(&scope, corrected)
        .await
        .unwrap();
    assert_eq!(corrected.coverage.planned, delayed.coverage.planned);
    assert_eq!(corrected.coverage.grounded_saved_analysis, 1);
    let replay = freeze_measurement_period(
        &scope,
        preview_measurement_period_at_cutoff(
            &scope,
            &delayed_window,
            None,
            vec![sample],
            None,
            analyzed,
            generated + Duration::minutes(2),
        )
        .unwrap(),
        2,
        Some(delayed.report_id),
    )
    .unwrap();
    assert_eq!(
        restarted
            .create_measurement_period(&scope, replay)
            .await
            .unwrap(),
        corrected
    );
    let reopened = PgReportRepository::from_database(&database);
    assert_eq!(
        reopened
            .get_measurement_period(&scope, delayed.report_id)
            .await
            .unwrap(),
        delayed
    );
    assert_eq!(
        reopened
            .get_measurement_period(&scope, corrected.report_id)
            .await
            .unwrap(),
        corrected
    );
    assert_eq!(
        restarted
            .get_measurement_period(&other, first.report_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        restarted
            .create_measurement_period(&other, first.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Forbidden
    );
    let cycles: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM optimization_cycles WHERE project_id=$1")
            .bind(project)
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert_eq!(cycles, 0);
    assert!(
        restarted
            .list(&scope, project.into())
            .await
            .unwrap()
            .is_empty()
    );
    use geo_domain::QuestionRepository;
    let questions = geo_persistence::PgQuestionRepository::from_database(&database);
    let command = geo_domain::CreateQuestionSet {
        idempotency_key: "scoped-report".into(),
        name: "Synthetic report questions".into(),
        questions: vec![geo_domain::QuestionDraft {
            question_id: None,
            text: "Synthetic question".into(),
            intent: "learn".into(),
            product_refs: vec![],
            market: "generic".into(),
            language: "en".into(),
            weight: 1,
            source: geo_domain::QuestionSource {
                kind: geo_domain::QuestionSourceKind::UserProvided,
                reference_id: None,
            },
        }],
    };
    let version = questions.create_set(&scope, command.clone()).await.unwrap();
    let selected = geo_domain::MeasurementReportQuestionSet {
        question_set_id: version.question_set_id,
        question_set_version_id: version.id,
    };
    let binding = questions
        .resolve_question(
            &scope,
            geo_domain::QuestionReference {
                question_set_id: version.question_set_id,
                question_set_version_id: version.id,
                question_id: version.questions[0].question_id,
                question_revision_id: version.questions[0].id,
            },
        )
        .await
        .unwrap()
        .binding;
    let mut scoped_sample = first.samples[0].clone();
    scoped_sample.question_binding = Some(binding);
    let scoped_preview = preview_measurement_period(
        &scope,
        &first.window(),
        Some(selected.clone()),
        vec![scoped_sample.clone()],
        analyzed,
    )
    .unwrap();
    let scoped = freeze_measurement_period(&scope, scoped_preview.clone(), 1, None).unwrap();
    let (saved, replay) = tokio::join!(
        restarted.create_measurement_period(&scope, scoped.clone()),
        restarted.create_measurement_period(&scope, scoped.clone())
    );
    assert_eq!(saved.unwrap(), replay.unwrap());
    assert_ne!(scoped.report_id, first.report_id);
    let correction =
        freeze_measurement_period(&scope, scoped_preview, 2, Some(scoped.report_id)).unwrap();
    restarted
        .create_measurement_period(&scope, correction)
        .await
        .unwrap();
    let next = questions
        .revise_set(
            &scope,
            version.question_set_id,
            geo_domain::ReviseQuestionSet {
                idempotency_key: "scoped-report-next".into(),
                base_version_id: version.id,
                name: command.name,
                questions: command.questions,
            },
        )
        .await
        .unwrap();
    let next_selected = geo_domain::MeasurementReportQuestionSet {
        question_set_id: next.question_set_id,
        question_set_version_id: next.id,
    };
    scoped_sample
        .question_binding
        .as_mut()
        .unwrap()
        .reference
        .question_set_version_id = next.id;
    scoped_sample
        .question_binding
        .as_mut()
        .unwrap()
        .reference
        .question_revision_id = next.questions[0].id;
    let next_preview = preview_measurement_period(
        &scope,
        &first.window(),
        Some(next_selected.clone()),
        vec![scoped_sample],
        analyzed,
    )
    .unwrap();
    let wrong_parent =
        freeze_measurement_period(&scope, next_preview.clone(), 2, Some(scoped.report_id)).unwrap();
    assert!(
        restarted
            .create_measurement_period(&scope, wrong_parent)
            .await
            .is_err()
    );
    let next_report = freeze_measurement_period(&scope, next_preview, 1, None).unwrap();
    restarted
        .create_measurement_period(&scope, next_report.clone())
        .await
        .unwrap();
    let reopened = PgReportRepository::from_database(&database);
    assert_eq!(
        reopened
            .get_measurement_period(&scope, scoped.report_id)
            .await
            .unwrap(),
        scoped
    );
    assert_eq!(
        reopened
            .list_measurement_periods(&scope, Some(selected.clone()))
            .await
            .unwrap()
            .len(),
        2
    );
    // Invalid snapshots in other report series must not be decoded by this
    // version's history query or by an exact-ID read of a valid snapshot.
    for question_set in [None, Some(next_selected.clone())] {
        let original_summaries = reopened
            .list_measurement_period_summaries(&scope, question_set.clone())
            .await
            .unwrap();
        let id = Uuid::new_v4();
        let mut malformed = serde_json::to_value(&next_report).unwrap();
        malformed["report_id"] = serde_json::json!(id);
        malformed["question_set"] = serde_json::json!(question_set);
        malformed["report_timezone"] = serde_json::json!("Etc/UTC");
        malformed["samples"][0]
            .as_object_mut()
            .unwrap()
            .remove("conditions");
        sqlx::query(
            "INSERT INTO measurement_period_reports (report_id,operator_id,tenant_id,project_id,revision,correction_of,report_window_start_at,report_window_end_at,report_timezone,evidence_as_of,snapshot,question_set_id,question_set_version_id) VALUES ($1,$2,$3,$4,1,NULL,$5,$6,'Etc/UTC',$7,$8,$9,$10)",
        ).bind(id).bind(operator).bind(tenant).bind(project)
            .bind(next_report.report_window_start_at).bind(next_report.report_window_end_at)
            .bind(next_report.evidence_as_of).bind(malformed.clone())
            .bind(question_set.as_ref().map(|selected| selected.question_set_id))
            .bind(question_set.as_ref().map(|selected| selected.question_set_version_id))
            .execute(database.pool()).await.unwrap();
        assert_eq!(
            reopened
                .list_measurement_periods(&scope, question_set.clone())
                .await
                .unwrap_err()
                .code,
            ErrorCode::Internal
        );
        // The catalog keeps every identity, even when its immutable body is invalid.
        let summaries = reopened
            .list_measurement_period_summaries(&scope, question_set.clone())
            .await
            .unwrap();
        assert_eq!(summaries.len(), original_summaries.len() + 1);
        assert!(original_summaries.iter().all(|row| summaries.contains(row)));
        let summary = summaries.iter().find(|row| row.report_id == id).unwrap();
        assert_eq!(summary.question_set, question_set);
        assert_eq!(summary.report_timezone, "Etc/UTC");
        assert_eq!(summary.project_id, scope.project_id.unwrap());
        assert_eq!(
            reopened
                .get_measurement_period(&scope, id)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Internal
        );
        let unchanged: serde_json::Value = sqlx::query_scalar(
            "SELECT snapshot FROM measurement_period_reports WHERE report_id=$1",
        )
        .bind(id)
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(unchanged, malformed);
    }
    let scoped_rows = reopened
        .list_measurement_periods(&scope, Some(selected.clone()))
        .await
        .unwrap();
    assert_eq!(scoped_rows.len(), 2);
    assert!(
        scoped_rows
            .iter()
            .all(|row| row.question_set.as_ref() == Some(&selected))
    );
    assert_eq!(
        reopened
            .get_measurement_period(&scope, scoped.report_id)
            .await
            .unwrap(),
        scoped
    );
    assert_eq!(
        reopened
            .get_measurement_period(&scope, first.report_id)
            .await
            .unwrap(),
        first
    );
    assert_eq!(
        reopened
            .create_measurement_period(&scope, scoped.clone())
            .await
            .unwrap(),
        scoped
    );
    // Save preflight reads only its exact window, even when strict history
    // cannot decode another window in the same question-set series.
    for question_set in [None, Some(next_selected.clone())] {
        let new_window = MeasurementPeriodWindow {
            start_at: first.report_window_start_at - Duration::days(1),
            ..first.window()
        };
        assert!(
            reopened
                .list_measurement_periods_in_window(&scope, question_set.clone(), &new_window)
                .await
                .unwrap()
                .is_empty()
        );
        let proposed = freeze_measurement_period(
            &scope,
            preview_measurement_period(&scope, &new_window, question_set.clone(), vec![], analyzed)
                .unwrap(),
            1,
            None,
        )
        .unwrap();
        let saved = reopened
            .create_measurement_period(&scope, proposed.clone())
            .await
            .unwrap();
        assert_eq!(
            reopened
                .create_measurement_period(&scope, proposed)
                .await
                .unwrap(),
            saved
        );
        assert_eq!(
            reopened
                .list_measurement_periods_in_window(&scope, question_set.clone(), &new_window)
                .await
                .unwrap(),
            vec![saved]
        );
        let corrupt_window = MeasurementPeriodWindow {
            report_timezone: "Etc/UTC".into(),
            ..first.window()
        };
        assert_eq!(
            reopened
                .list_measurement_periods_in_window(&scope, question_set.clone(), &corrupt_window)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Internal
        );
        let corrupt_replay = freeze_measurement_period(
            &scope,
            preview_measurement_period(&scope, &corrupt_window, question_set, vec![], analyzed)
                .unwrap(),
            1,
            None,
        )
        .unwrap();
        assert_eq!(
            reopened
                .create_measurement_period(&scope, corrupt_replay)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Internal
        );
    }
    let exact = reopened
        .list_measurement_periods_in_window(&scope, Some(selected.clone()), &first.window())
        .await
        .unwrap();
    assert_eq!(exact.len(), 2);
    assert!(
        exact
            .iter()
            .all(|row| row.question_set.as_ref() == Some(&selected))
    );
    let summaries = reopened
        .list_measurement_period_summaries(&scope, Some(selected.clone()))
        .await
        .unwrap();
    assert_eq!(
        summaries,
        exact
            .iter()
            .rev()
            .map(geo_domain::MeasurementPeriodReportSummary::from)
            .collect::<Vec<_>>()
    );
    assert!(
        reopened
            .list_measurement_period_summaries(&other, Some(selected.clone()))
            .await
            .unwrap()
            .is_empty()
    );
    let mut unscoped = scope.clone();
    unscoped.project_id = None;
    assert_eq!(
        reopened
            .list_measurement_period_summaries(&unscoped, None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Forbidden
    );
    assert!(
        reopened
            .list_measurement_periods_in_window(&other, Some(selected), &first.window())
            .await
            .unwrap()
            .is_empty()
    );
    let mut invalid = next_report;
    invalid.report_id = Uuid::new_v4();
    invalid
        .question_set
        .as_mut()
        .unwrap()
        .question_set_version_id = Uuid::new_v4();
    assert!(
        reopened
            .create_measurement_period(&scope, invalid)
            .await
            .is_err()
    );
}
