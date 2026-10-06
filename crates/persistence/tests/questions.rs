//! Run only with a disposable GEO_TEST_DATABASE_URL; these tests migrate and write.
use geo_domain::{
    CreateQuestionSet, QuestionDraft, QuestionPurpose, QuestionReference, QuestionRepository,
    QuestionSource, QuestionSourceKind, ReviseQuestionSet, TenantScope,
    create_question_request_hash,
};
use geo_persistence::{Database, DatabaseConfig, PgQuestionRepository};
use uuid::Uuid;

async fn fixture(database: &Database) -> TenantScope {
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    let project = Uuid::new_v4();
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'Test')")
        .bind(operator)
        .bind(format!("questions-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,'Test')",
    )
    .bind(tenant)
    .bind(operator)
    .bind(format!("questions-{tenant}"))
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO projects \
         (project_id,operator_id,tenant_id,slug,display_name) \
         VALUES ($1,$2,$3,$4,'Test')",
    )
    .bind(project)
    .bind(operator)
    .bind(tenant)
    .bind(format!("questions-{project}"))
    .execute(database.pool())
    .await
    .unwrap();
    TenantScope::new(operator.into(), tenant.into(), Some(project.into()))
}

fn draft(number: u32) -> QuestionDraft {
    QuestionDraft {
        question_id: None,
        text: format!("What does example feature {number} do?"),
        intent: "informational".into(),
        product_refs: vec![],
        market: "global".into(),
        language: "en".into(),
        source: QuestionSource {
            kind: QuestionSourceKind::UserProvided,
            reference_id: None,
        },
        weight: 50,
    }
}

fn create(key: &str, start: u32, count: u32) -> CreateQuestionSet {
    CreateQuestionSet {
        idempotency_key: key.into(),
        name: format!("Example set {start}"),
        questions: (start..start + count).map(draft).collect(),
    }
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn question_versions_registry_aliases_and_restart_are_scoped() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database URL");
    let database = Database::connect_and_migrate(&DatabaseConfig::from_url(url).unwrap())
        .await
        .unwrap();
    let scope = fixture(&database).await;
    let foreign = fixture(&database).await;
    let sibling_project = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO projects (project_id,operator_id,tenant_id,slug,display_name) \
         VALUES ($1,$2,$3,$4,'Test')",
    )
    .bind(sibling_project)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(format!("questions-{sibling_project}"))
    .execute(database.pool())
    .await
    .unwrap();
    let sibling = TenantScope::new(
        scope.operator_id,
        scope.tenant_id,
        Some(sibling_project.into()),
    );
    let repo = PgQuestionRepository::from_database(&database);
    let first_command = create("first-create", 0, 5);
    let first = repo
        .create_set(&scope, first_command.clone())
        .await
        .unwrap();
    assert_eq!(first.revision, 1);
    assert_eq!(first.evaluation_count, 1);
    assert_eq!(
        repo.create_set(&scope, first_command.clone())
            .await
            .unwrap(),
        first
    );
    let hash = create_question_request_hash(&first_command).unwrap();
    assert_eq!(
        repo.replay(&scope, &first_command.idempotency_key, &hash)
            .await
            .unwrap(),
        Some(first.clone())
    );
    assert!(
        repo.replay(&scope, &first_command.idempotency_key, "wrong-hash")
            .await
            .is_err()
    );
    assert!(
        repo.replay(&foreign, &first_command.idempotency_key, &hash)
            .await
            .unwrap()
            .is_none()
    );
    let mut changed_same_key = first_command;
    changed_same_key.name.push_str(" changed");
    assert!(repo.create_set(&scope, changed_same_key).await.is_err());

    let second_command = CreateQuestionSet {
        idempotency_key: "reuse-create".into(),
        name: "Another set".into(),
        questions: vec![
            QuestionDraft {
                text: format!("  {}  ", first.questions[0].text.to_uppercase()),
                ..draft(0)
            },
            draft(5),
        ],
    };
    let second = repo.create_set(&scope, second_command).await.unwrap();
    assert_eq!(
        second.questions[0].question_id,
        first.questions[0].question_id
    );
    assert_eq!(second.questions[0].purpose, first.questions[0].purpose);

    let original = first.questions[0].clone();
    let edit = QuestionDraft {
        question_id: Some(original.question_id),
        text: "What does the revised example feature do?".into(),
        ..draft(0)
    };
    let revise = ReviseQuestionSet {
        idempotency_key: "first-revision".into(),
        base_version_id: first.id,
        name: "Edited set".into(),
        questions: std::iter::once(edit.clone())
            .chain(first.questions[1..].iter().map(|question| QuestionDraft {
                question_id: Some(question.question_id),
                text: question.text.clone(),
                intent: question.intent.clone(),
                product_refs: question.product_refs.clone(),
                market: question.market.clone(),
                language: question.language.clone(),
                source: question.source.clone(),
                weight: question.weight,
            }))
            .collect(),
    };
    let revised = repo
        .revise_set(&scope, first.question_set_id, revise.clone())
        .await
        .unwrap();
    assert_eq!(revised.revision, 2);
    assert_eq!(revised.parent_version_id, Some(first.id));
    assert_eq!(revised.questions[0].question_id, original.question_id);
    assert_eq!(revised.questions[0].purpose, original.purpose);
    assert_ne!(revised.questions[0].id, original.id);
    let restarted = PgQuestionRepository::from_database(&database);
    assert_eq!(
        restarted
            .revise_set(&scope, first.question_set_id, revise.clone())
            .await
            .unwrap(),
        revised
    );
    let stale = ReviseQuestionSet {
        idempotency_key: "stale-revision".into(),
        ..revise
    };
    assert!(
        restarted
            .revise_set(&scope, first.question_set_id, stale)
            .await
            .is_err()
    );
    let collision = ReviseQuestionSet {
        idempotency_key: "other-identity-alias".into(),
        base_version_id: revised.id,
        name: "Invalid alias".into(),
        questions: vec![QuestionDraft {
            text: first.questions[1].text.clone(),
            question_id: Some(original.question_id),
            ..edit
        }],
    };
    assert!(
        restarted
            .revise_set(&scope, first.question_set_id, collision)
            .await
            .is_err()
    );

    let old = restarted
        .get_version(&scope, first.question_set_id, first.id)
        .await
        .unwrap();
    assert_eq!(old, first);
    let reference = QuestionReference {
        question_set_id: first.question_set_id,
        question_set_version_id: first.id,
        question_id: original.question_id,
        question_revision_id: original.id,
    };
    let resolved = restarted.resolve_question(&scope, reference).await.unwrap();
    assert_eq!(resolved.revision, original);
    assert_eq!(resolved.binding.purpose, original.purpose);
    assert!(
        restarted
            .resolve_question(
                &scope,
                QuestionReference {
                    question_set_version_id: revised.id,
                    ..reference
                },
            )
            .await
            .is_err()
    );
    assert!(
        restarted
            .get_version(&foreign, first.question_set_id, first.id)
            .await
            .is_err()
    );
    assert!(
        restarted
            .resolve_question(&foreign, reference)
            .await
            .is_err()
    );
    assert!(
        restarted
            .get_version(&sibling, first.question_set_id, first.id)
            .await
            .is_err()
    );
    assert!(
        restarted
            .resolve_question(&sibling, reference)
            .await
            .is_err()
    );
    assert!(
        restarted
            .revise_set(
                &sibling,
                first.question_set_id,
                ReviseQuestionSet {
                    idempotency_key: "foreign-set".into(),
                    base_version_id: first.id,
                    name: "Foreign edit".into(),
                    questions: vec![draft(70)],
                },
            )
            .await
            .is_err()
    );
    let first_page = restarted.list_sets(&scope, None, 1).await.unwrap();
    assert_eq!(first_page.items.len(), 1);
    assert!(first_page.next_cursor.is_some());
    let last_page = restarted
        .list_sets(&scope, first_page.next_cursor, 1)
        .await
        .unwrap();
    assert_eq!(last_page.items.len(), 1);
    assert!(last_page.next_cursor.is_none());
    let versions = restarted
        .list_versions(&scope, first.question_set_id, None, 1)
        .await
        .unwrap();
    assert_eq!(versions.items[0].revision, 1);
    assert_eq!(versions.next_cursor, Some(1));
    let old_alias_rejoined = restarted
        .create_set(
            &scope,
            CreateQuestionSet {
                idempotency_key: "rejoin-original-alias".into(),
                name: "Historical spelling".into(),
                questions: vec![QuestionDraft {
                    text: original.text.clone(),
                    ..draft(0)
                }],
            },
        )
        .await
        .unwrap();
    assert_eq!(
        old_alias_rejoined.questions[0].question_id,
        original.question_id
    );
    assert_eq!(old_alias_rejoined.questions[0].purpose, original.purpose);

    let project = scope.project_id.unwrap().as_uuid();
    let aliases: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM question_identity_aliases \
         WHERE project_id=$1 AND question_id=$2",
    )
    .bind(project)
    .bind(original.question_id)
    .fetch_one(database.pool())
    .await
    .unwrap();
    assert_eq!(aliases, 2);
    let members: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM question_version_members \
         WHERE project_id=$1 AND question_set_version_id=$2",
    )
    .bind(project)
    .bind(revised.id)
    .fetch_one(database.pool())
    .await
    .unwrap();
    assert_eq!(members, 5);
    assert!(
        sqlx::query(
            "UPDATE question_identities SET evaluation_split='optimization' \
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND question_id=$4",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project)
        .bind(original.question_id)
        .execute(database.pool())
        .await
        .is_err()
    );
    assert!(
        sqlx::query(
            "UPDATE question_set_versions SET name='rewritten' \
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 \
           AND question_set_id=$4 AND question_set_version_id=$5",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project)
        .bind(first.question_set_id)
        .bind(first.id)
        .execute(database.pool())
        .await
        .is_err()
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn concurrent_registration_serializes_project_split_and_idempotency() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database URL");
    let database = Database::connect_and_migrate(&DatabaseConfig::from_url(url).unwrap())
        .await
        .unwrap();
    let scope = fixture(&database).await;
    let repo = PgQuestionRepository::from_database(&database);
    let (left, right) = tokio::join!(
        repo.create_set(&scope, create("concurrent-left", 20, 5)),
        repo.create_set(&scope, create("concurrent-right", 25, 5)),
    );
    let (left, right) = (left.unwrap(), right.unwrap());
    assert_eq!(left.evaluation_count, 1);
    assert_eq!(right.evaluation_count, 1);
    let count: i64 = sqlx::query_scalar(
        "SELECT registered_count FROM question_project_registries \
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(scope.project_id.unwrap().as_uuid())
    .fetch_one(database.pool())
    .await
    .unwrap();
    assert_eq!(count, 10);
    let (first_retry, simultaneous_retry) = tokio::join!(
        repo.create_set(&scope, create("simultaneous-key", 30, 1)),
        repo.create_set(&scope, create("simultaneous-key", 30, 1)),
    );
    assert_eq!(first_retry.unwrap(), simultaneous_retry.unwrap());
    let count_after_retry: i64 = sqlx::query_scalar(
        "SELECT registered_count FROM question_project_registries \
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(scope.project_id.unwrap().as_uuid())
    .fetch_one(database.pool())
    .await
    .unwrap();
    assert_eq!(count_after_retry, 11);
    let duplicate = repo
        .create_set(&scope, create("same-text-in-new-set", 20, 1))
        .await
        .unwrap();
    assert_eq!(
        duplicate.questions[0].question_id,
        left.questions[0].question_id
    );
    assert_eq!(duplicate.questions[0].purpose, left.questions[0].purpose);
    assert!(
        left.questions
            .iter()
            .chain(right.questions.iter())
            .filter(|question| question.purpose == QuestionPurpose::FrozenEvaluation)
            .count()
            == 2
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn long_unicode_aliases_remain_indexed_and_reusable() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database URL");
    let database = Database::connect_and_migrate(&DatabaseConfig::from_url(url).unwrap())
        .await
        .unwrap();
    let scope = fixture(&database).await;
    let repo = PgQuestionRepository::from_database(&database);
    // More than the btree index's inline key limit, yet within the domain
    // contract's 2,000 Unicode character limit.
    let text: String = (0..1900)
        .map(|index| char::from_u32(0x4e00 + index).unwrap())
        .collect();
    let first = repo
        .create_set(
            &scope,
            CreateQuestionSet {
                idempotency_key: "long-unicode-first".into(),
                name: "Long question".into(),
                questions: vec![QuestionDraft {
                    text: text.clone(),
                    ..draft(100)
                }],
            },
        )
        .await
        .unwrap();
    let second = repo
        .create_set(
            &scope,
            CreateQuestionSet {
                idempotency_key: "long-unicode-reuse".into(),
                name: "Long question in another set".into(),
                questions: vec![QuestionDraft { text, ..draft(100) }],
            },
        )
        .await
        .unwrap();
    assert_eq!(
        first.questions[0].question_id,
        second.questions[0].question_id
    );
    assert_eq!(first.questions[0].purpose, second.questions[0].purpose);
    assert_eq!(second.evaluation_count, 1);
}
