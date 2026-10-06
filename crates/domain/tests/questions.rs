use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::Utc;
use geo_domain::{
    CreateQuestionSet, ErrorCode, MemoryQuestionRepository, OperatorId, ProjectId, QuestionDraft,
    QuestionProjectState, QuestionPurpose, QuestionReference, QuestionRepository, QuestionSource,
    QuestionSourceKind, ReviseQuestionSet, TenantId, TenantScope, create_question_request_hash,
    normalize_question_text,
};
use uuid::Uuid;

fn scope(operator: Uuid, tenant: Uuid, project: Uuid) -> TenantScope {
    TenantScope::new(
        OperatorId(operator),
        TenantId(tenant),
        Some(ProjectId(project)),
    )
}

fn draft(text: &str) -> QuestionDraft {
    QuestionDraft {
        question_id: None,
        text: text.to_owned(),
        intent: "selection".to_owned(),
        product_refs: vec![],
        market: "global".to_owned(),
        language: "en".to_owned(),
        source: QuestionSource {
            kind: QuestionSourceKind::UserProvided,
            reference_id: None,
        },
        weight: 1,
    }
}

fn create(key: &str, name: &str, texts: &[&str]) -> CreateQuestionSet {
    CreateQuestionSet {
        idempotency_key: key.to_owned(),
        name: name.to_owned(),
        questions: texts.iter().map(|text| draft(text)).collect(),
    }
}

#[test]
fn normalization_is_nfkc_lowercase_and_whitespace_only() {
    assert_eq!(normalize_question_text("  Ａ\tＢ　C\n"), "a b c");
    assert_eq!(normalize_question_text("What's?"), "what's?");
    assert_ne!(normalize_question_text("a!"), normalize_question_text("a"));
    assert_eq!(normalize_question_text("İ"), "i\u{307}");
}

#[test]
fn split_ceil_is_project_wide_and_row_order_independent() {
    let seed = Uuid::from_u128(177);
    let mut first = QuestionProjectState::new(seed);
    let initial = first
        .create_set(create("one", "first", &["one"]), Utc::now())
        .unwrap();
    assert_eq!(
        (initial.evaluation_count, initial.optimization_count),
        (1, 0)
    );
    let duplicate = first
        .create_set(create("copy", "other set", &["ＯＮＥ"]), Utc::now())
        .unwrap();
    assert_eq!(
        duplicate.questions[0].question_id,
        initial.questions[0].question_id
    );
    assert_eq!(first.enrolled_count, 1);
    let five = first
        .create_set(
            create("next", "batch", &["two", "three", "four", "five"]),
            Utc::now(),
        )
        .unwrap();
    assert_eq!(five.evaluation_count, 0);
    assert_eq!(first.enrolled_count, 5);
    let six = first
        .create_set(create("six", "batch", &["six"]), Utc::now())
        .unwrap();
    assert_eq!(six.evaluation_count, 1);
    let ten = first
        .create_set(
            create("ten", "batch", &["seven", "eight", "nine", "ten"]),
            Utc::now(),
        )
        .unwrap();
    assert_eq!(ten.evaluation_count, 0);
    assert_eq!(first.enrolled_count, 10);

    let rows = ["a", "b", "c", "d", "e", "f", "g"];
    let mut forward = QuestionProjectState::new(seed);
    let mut reverse = QuestionProjectState::new(seed);
    let original = forward
        .create_set(create("a", "set", &rows), Utc::now())
        .unwrap();
    let reversed = reverse
        .create_set(
            create("a", "set", &rows.iter().rev().copied().collect::<Vec<_>>()),
            Utc::now(),
        )
        .unwrap();
    let as_map = |questions: &[geo_domain::QuestionRevision]| -> HashMap<String, QuestionPurpose> {
        questions
            .iter()
            .map(|question: &geo_domain::QuestionRevision| {
                (question.text.clone(), question.purpose)
            })
            .collect()
    };
    assert_eq!(as_map(&original.questions), as_map(&reversed.questions));
    assert_eq!(original.evaluation_count, 2);
}

#[test]
fn compact_project_state_uses_persisted_count_without_loading_history() {
    let mut compact = QuestionProjectState::from_parts(
        Uuid::from_u128(19),
        10,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    let version = compact
        .create_set(
            create("fresh", "new", &["eleven", "twelve", "thirteen"]),
            Utc::now(),
        )
        .unwrap();
    assert_eq!(
        (version.evaluation_count, version.optimization_count),
        (1, 2)
    );
    assert_eq!(compact.enrolled_count, 13);
    assert_eq!(compact.identities.len(), 3);
    let restored: QuestionProjectState =
        serde_json::from_value(serde_json::to_value(&compact).unwrap()).unwrap();
    assert_eq!(restored, compact);
}

#[test]
fn edits_reserve_historical_aliases_and_never_flip_purpose() {
    let mut state = QuestionProjectState::new(Uuid::from_u128(7));
    let v1 = state
        .create_set(create("c", "set", &["original", "another"]), Utc::now())
        .unwrap();
    let original = &v1.questions[0];
    let v2 = state
        .revise_set(
            v1.question_set_id,
            ReviseQuestionSet {
                idempotency_key: "edit".into(),
                base_version_id: v1.id,
                name: "set".into(),
                questions: vec![QuestionDraft {
                    question_id: Some(original.question_id),
                    text: "renamed".into(),
                    ..draft("renamed")
                }],
            },
            Utc::now(),
        )
        .unwrap();
    assert_ne!(v1.questions[0].id, v2.questions[0].id);
    assert_eq!(v1.questions[0].purpose, v2.questions[0].purpose);
    assert_eq!(state.enrolled_count, 2);
    let copy = state
        .create_set(create("copy", "copy", &["original"]), Utc::now())
        .unwrap();
    assert_eq!(copy.questions[0].question_id, original.question_id);
    let old_revision = state.get_version(v1.question_set_id, v1.id).unwrap();
    assert_eq!(old_revision.questions[0].text, "original");

    let v3 = state
        .revise_set(
            v1.question_set_id,
            ReviseQuestionSet {
                idempotency_key: "remove".into(),
                base_version_id: v2.id,
                name: "set".into(),
                questions: vec![draft("another")],
            },
            Utc::now(),
        )
        .unwrap();
    let v4 = state
        .revise_set(
            v1.question_set_id,
            ReviseQuestionSet {
                idempotency_key: "rejoin".into(),
                base_version_id: v3.id,
                name: "set".into(),
                questions: vec![draft("renamed")],
            },
            Utc::now(),
        )
        .unwrap();
    assert_eq!(v4.questions[0].question_id, original.question_id);
    assert_eq!(v4.questions[0].purpose, original.purpose);
    assert_eq!(state.enrolled_count, 2);
}

#[test]
fn conflicting_aliases_rollback_without_enrollment() {
    let mut state = QuestionProjectState::new(Uuid::new_v4());
    let v1 = state
        .create_set(create("start", "set", &["alpha", "beta"]), Utc::now())
        .unwrap();
    let before = state.clone();
    let error = state
        .revise_set(
            v1.question_set_id,
            ReviseQuestionSet {
                idempotency_key: "conflict".into(),
                base_version_id: v1.id,
                name: "set".into(),
                questions: vec![QuestionDraft {
                    question_id: Some(v1.questions[0].question_id),
                    ..draft("beta")
                }],
            },
            Utc::now(),
        )
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(state, before);

    let error = state
        .create_set(create("dupes", "set", &["Ａ", "a"]), Utc::now())
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(state, before);
}

#[test]
fn stale_parent_replay_and_key_conflict() {
    let mut state = QuestionProjectState::new(Uuid::new_v4());
    let first = create("same", "set", &["first"]);
    let v1 = state.create_set(first.clone(), Utc::now()).unwrap();
    assert_eq!(state.create_set(first, Utc::now()).unwrap(), v1);
    assert_eq!(
        state
            .create_set(create("same", "set", &["different"]), Utc::now())
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let edit = ReviseQuestionSet {
        idempotency_key: "revise".into(),
        base_version_id: v1.id,
        name: "renamed".into(),
        questions: vec![draft("second")],
    };
    let v2 = state
        .revise_set(v1.question_set_id, edit.clone(), Utc::now())
        .unwrap();
    assert_eq!(
        state
            .revise_set(v1.question_set_id, edit, Utc::now())
            .unwrap(),
        v2
    );
    assert_eq!(
        state
            .revise_set(
                v1.question_set_id,
                ReviseQuestionSet {
                    idempotency_key: "stale".into(),
                    base_version_id: v1.id,
                    name: "set".into(),
                    questions: vec![draft("third")],
                },
                Utc::now()
            )
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(state.enrolled_count, 2);
}

#[tokio::test]
async fn memory_repository_scopes_exact_membership_and_concurrent_enrollment() {
    let repository = Arc::new(MemoryQuestionRepository::default());
    let a = scope(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let b = scope(a.operator_id.0, a.tenant_id.0, Uuid::new_v4());
    let c = scope(a.operator_id.0, Uuid::new_v4(), a.project_id.unwrap().0);
    let d = scope(Uuid::new_v4(), a.tenant_id.0, a.project_id.unwrap().0);
    let mut tasks = Vec::new();
    for i in 0..10 {
        let repo = repository.clone();
        let scope = a.clone();
        tasks.push(tokio::spawn(async move {
            let text = format!("concurrent {i}");
            repo.create_set(&scope, create(&format!("key-{i}"), "batch", &[&text]))
                .await
                .unwrap()
        }));
    }
    let mut versions = Vec::new();
    for task in tasks {
        versions.push(task.await.unwrap());
    }
    assert_eq!(
        versions
            .iter()
            .map(|version| version.evaluation_count)
            .sum::<u32>(),
        2
    );
    let first = &versions[0];
    let reference = QuestionReference {
        question_set_id: first.question_set_id,
        question_set_version_id: first.id,
        question_id: first.questions[0].question_id,
        question_revision_id: first.questions[0].id,
    };
    let resolved = repository.resolve_question(&a, reference).await.unwrap();
    assert_eq!(resolved.binding.reference, reference);
    assert_eq!(resolved.revision.text, first.questions[0].text);
    for wrong in [&b, &c, &d] {
        assert_eq!(
            repository
                .resolve_question(wrong, reference)
                .await
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
        assert!(
            repository
                .list_sets(wrong, None, 100)
                .await
                .unwrap()
                .items
                .is_empty()
        );
    }
    assert_eq!(
        repository
            .resolve_question(
                &a,
                QuestionReference {
                    question_revision_id: Uuid::new_v4(),
                    ..reference
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        repository
            .resolve_question(
                &a,
                QuestionReference {
                    question_set_version_id: versions[1].id,
                    ..reference
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        repository.list_sets(&a, None, 0).await.unwrap_err().code,
        ErrorCode::InvalidRequest
    );
    let mut cursor = None;
    let mut seen = HashSet::new();
    loop {
        let page = repository.list_sets(&a, cursor, 3).await.unwrap();
        for item in &page.items {
            assert!(seen.insert(item.id));
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen.len(), 10);
}

#[tokio::test]
async fn replay_preflight_reads_historical_version_without_revalidating_mutable_sources() {
    let repo = MemoryQuestionRepository::default();
    let s = scope(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let other = scope(s.operator_id.0, s.tenant_id.0, Uuid::new_v4());
    let original = create("stable-key", "set", &["original"]);
    let hash = create_question_request_hash(&original).unwrap();
    let first = repo.create_set(&s, original).await.unwrap();
    let current = repo
        .revise_set(
            &s,
            first.question_set_id,
            ReviseQuestionSet {
                idempotency_key: "newer".into(),
                base_version_id: first.id,
                name: "changed".into(),
                questions: vec![draft("new question")],
            },
        )
        .await
        .unwrap();
    assert_ne!(first.id, current.id);
    assert_eq!(
        repo.replay(&s, "stable-key", &hash).await.unwrap(),
        Some(first)
    );
    assert_eq!(
        repo.replay(&other, "stable-key", &hash).await.unwrap(),
        None
    );
    assert_eq!(
        repo.replay(&s, "stable-key", "altered")
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(repo.replay(&s, "missing-key", &hash).await.unwrap(), None);
}

#[test]
fn strict_dto_rejects_forged_split_or_scope() {
    let request = serde_json::json!({
        "idempotency_key": "key",
        "name": "set",
        "questions": [{
            "text":"hello", "intent":"selection", "product_refs": [],
            "market":"global", "language":"en", "source":{"kind":"user_provided"},
            "weight":1, "purpose":"optimization"
        }]
    });
    assert!(serde_json::from_value::<CreateQuestionSet>(request).is_err());
    let request = serde_json::json!({
        "question_set_id": Uuid::new_v4(), "question_set_version_id":Uuid::new_v4(),
        "question_id":Uuid::new_v4(), "question_revision_id":Uuid::new_v4(),
        "purpose":"optimization"
    });
    assert!(serde_json::from_value::<QuestionReference>(request).is_err());
}
