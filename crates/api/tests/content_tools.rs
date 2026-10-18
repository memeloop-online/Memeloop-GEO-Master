use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use geo_api::{
    AppState, EventBus, MemoryIdempotencyStore, MemoryOperationStore, ModelProviderBridge,
    RepositoryHostOps, content_runtime::ContentWorkflowExecutor,
};
use geo_domain::{
    ContentEvidence, ContentItemStatus, ContentRepository, DocumentScope, EvidenceRef, ImportItem,
    InitialSource, InitialSourceKind, InitialSourceVisibility, KnowledgePurpose,
    KnowledgeRepository, MemoryAuthRepository, MemoryContentRepository, MemoryKnowledgeRepository,
    MemoryProjectRepository, ProjectCreate, ProjectPatch, ProjectRepository, ProjectSettings,
    ProjectStartCommand, ReviseSourceTextCommand, SourceKind, TenantScope, hash_idempotency_key,
    settings_hash, start_request_hash,
};
use geo_worker::{
    ContentCheckBeginResult, ContentCheckFinishRequest, ContentCloseRequest,
    ContentExecutionReadRequest, ContentGenerateBeginResult, ContentGenerateFinishRequest,
    ContentItemRef, ContentItemsReadRequest, ContentModelOutcome, ContentPrepareRequest,
    ContentRepairBeginResult, ContentRepairFinishRequest, ContentSourceReadRequest,
    ContentStartRequest, ContentStepRequest, HostOpError, HostOpErrorCode, HostOps,
    ModelCompletion, ModelCompletionRequest,
};
use uuid::Uuid;

struct AcceptedExecutor(AtomicUsize);
impl ContentWorkflowExecutor for AcceptedExecutor {
    fn dispatch(
        &self,
        _scope: TenantScope,
        _execution_id: Uuid,
    ) -> Result<(), geo_domain::AppError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct GroundedModel(AtomicUsize, AtomicUsize);

async fn generate_selected(
    host: &RepositoryHostOps,
    scope: &TenantScope,
    step: ContentStepRequest,
) -> ContentItemRef {
    let ContentGenerateBeginResult::Claimed { token, descriptor } = host
        .content_generate_begin(scope, step.clone())
        .await
        .unwrap()
    else {
        panic!("a prepared fixture must claim generation");
    };
    let output = host
        .model_complete(
            scope,
            ModelCompletionRequest {
                prompt: serde_json::json!({"evidence": descriptor.evidence.iter().map(|e| serde_json::json!({
                    "chunk_id": e.reference.chunk_id, "quote": e.exact_quote
                })).collect::<Vec<_>>()}).to_string(),
                system: None,
                model: None,
                max_output_tokens: None,
                messages: vec![],
                tools: vec![],
            },
        )
        .await
        .unwrap();
    host.content_generate_finish(
        scope,
        ContentGenerateFinishRequest {
            execution_id: step.execution_id,
            item_id: step.item_id,
            token,
            outcome: ContentModelOutcome::Completed { text: output.text },
        },
    )
    .await
    .unwrap()
}

async fn check_selected(
    host: &RepositoryHostOps,
    scope: &TenantScope,
    step: ContentStepRequest,
) -> (ContentItemRef, ContentCheckFinishRequest) {
    let ContentCheckBeginResult::Claimed {
        token,
        revision,
        quotes,
        title_check_id,
        check_sections,
    } = host.content_check_begin(scope, step.clone()).await.unwrap()
    else {
        panic!("a drafted fixture must claim its independent check");
    };
    let output = host
        .model_complete(
            scope,
            ModelCompletionRequest {
                prompt: serde_json::json!({
                    "document": revision.document, "title_check_id": title_check_id,
                    "check_sections": check_sections,
                    "evidence": quotes.iter().map(|e| serde_json::json!({
                        "chunk_id": e.reference.chunk_id, "quote": e.exact_quote
                    })).collect::<Vec<_>>()
                })
                .to_string(),
                system: Some("independent factual checker".into()),
                model: None,
                max_output_tokens: None,
                messages: vec![],
                tools: vec![],
            },
        )
        .await
        .unwrap();
    let request = ContentCheckFinishRequest {
        execution_id: step.execution_id,
        item_id: step.item_id,
        token,
        outcome: ContentModelOutcome::Completed { text: output.text },
    };
    let item = host
        .content_check_finish(scope, request.clone())
        .await
        .unwrap();
    assert_eq!(
        host.content_check_finish(scope, request.clone())
            .await
            .unwrap(),
        item,
        "replaying a completed check token must not create another check"
    );
    (item, request)
}

async fn repair_selected(
    host: &RepositoryHostOps,
    scope: &TenantScope,
    step: ContentStepRequest,
) -> (ContentItemRef, ContentRepairFinishRequest) {
    let ContentRepairBeginResult::Claimed {
        token,
        revision,
        quotes,
        blocking_findings,
        check_sections,
    } = host
        .content_repair_begin(scope, step.clone())
        .await
        .unwrap()
    else {
        panic!("the fixture must claim a blocked revision for repair");
    };
    let output = host
        .model_complete(
            scope,
            ModelCompletionRequest {
                prompt: serde_json::json!({
                    "previous_document": revision.document,
                    "blocking_findings": blocking_findings, "check_sections": check_sections,
                    "evidence": quotes.iter().map(|e| serde_json::json!({
                        "chunk_id": e.reference.chunk_id, "quote": e.exact_quote
                    })).collect::<Vec<_>>()
                })
                .to_string(),
                system: Some("source-grounded factual repairer".into()),
                model: None,
                max_output_tokens: None,
                messages: vec![],
                tools: vec![],
            },
        )
        .await
        .unwrap();
    let request = ContentRepairFinishRequest {
        execution_id: step.execution_id,
        item_id: step.item_id,
        token,
        outcome: ContentModelOutcome::Completed { text: output.text },
    };
    let item = host
        .content_repair_finish(scope, request.clone())
        .await
        .unwrap();
    assert_eq!(
        host.content_repair_finish(scope, request.clone())
            .await
            .unwrap(),
        item,
        "replaying a completed repair token must not create another revision"
    );
    (item, request)
}

async fn selected_prepare(
    host: &RepositoryHostOps,
    scope: &TenantScope,
    step: &ContentStepRequest,
) -> ContentPrepareRequest {
    let items = host
        .content_items_read(
            scope,
            ContentItemsReadRequest {
                execution_id: step.execution_id,
                cursor: None,
                limit: Some(100),
            },
        )
        .await
        .unwrap();
    let item = items
        .items
        .iter()
        .find(|item| item.item_id == step.item_id)
        .unwrap();
    let source = &item.source_refs[0];
    let page = host
        .content_source_read(
            scope,
            ContentSourceReadRequest {
                execution_id: step.execution_id,
                item_id: step.item_id,
                source_id: source.source_id,
                source_version_id: source.source_version_id,
                after_ordinal: None,
                limit: 1,
            },
        )
        .await
        .unwrap();
    let chunk = &page.chunks[0];
    ContentPrepareRequest {
        execution_id: step.execution_id,
        item_id: step.item_id,
        evidence: vec![ContentEvidence {
            reference: EvidenceRef {
                source_version_id: source.source_version_id,
                chunk_id: Some(chunk.chunk_id),
                locator: chunk.locator.clone(),
            },
            exact_quote: chunk.text.clone(),
        }],
    }
}
#[async_trait]
impl ModelProviderBridge for GroundedModel {
    async fn complete(
        &self,
        _scope: &TenantScope,
        request: &ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let input: serde_json::Value = serde_json::from_str(&request.prompt).unwrap();
        let citation = input["evidence"][0]["chunk_id"].as_str().unwrap();
        let text = if request
            .system
            .as_deref()
            .unwrap_or_default()
            .contains("checker")
        {
            let unsupported = self
                .1
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok();
            let block = input["document"]["blocks"][0]["block_id"].as_str().unwrap();
            let title = input["title_check_id"].as_str().unwrap();
            serde_json::json!({"checks":[
                {"block_id":title,"verdict":"supported","citation_ids":[citation],
                    "detail":"Source quote supports title"},
                {"block_id":block,"verdict":if unsupported {"unsupported"} else {"supported"},"citation_ids":[citation],
                    "detail":"Source quote supports statement"}]})
            .to_string()
        } else if request
            .system
            .as_deref()
            .unwrap_or_default()
            .contains("repairer")
        {
            let mut document = input["previous_document"].clone();
            document["blocks"][0]["text"] = input["evidence"][0]["quote"].clone();
            document.to_string()
        } else {
            let text = if self.1.load(Ordering::SeqCst) > 0 {
                "Public description. Unsupported draft sentence."
            } else {
                "Public description"
            };
            serde_json::json!({"schema_version":2,"title":"A documented answer","blocks":[{"kind":"paragraph",
                "text":text,"citation_ids":[citation],"items":[]}]})
            .to_string()
        };
        Ok(ModelCompletion {
            text,
            tool_calls: vec![],
            model: "injected".into(),
            prompt_tokens: 1,
            completion_tokens: 1,
            finish_reason: "stop".into(),
        })
    }
}

#[tokio::test]
async fn host_ops_use_scoped_references_and_reject_foreign_or_tampered_cursor() {
    let tenant = TenantScope::new(Uuid::new_v4().into(), Uuid::new_v4().into(), None);
    let projects = Arc::new(MemoryProjectRepository::default());
    let project = projects
        .create(
            &tenant,
            ProjectCreate {
                slug: None,
                display_name: "Host content".into(),
                settings: ProjectSettings {
                    brand_name: "Example".into(),
                    market: "US".into(),
                    language: "en".into(),
                    initial_sources: vec![InitialSource {
                        kind: InitialSourceKind::Text,
                        value: "Public description".into(),
                        visibility: InitialSourceVisibility::Public,
                        version_ref: None,
                        content_hash: None,
                    }],
                    document_scope: DocumentScope {
                        content_types: vec!["faq".into(), "company_profile".into()],
                        ..DocumentScope::default()
                    },
                    ..ProjectSettings::default()
                },
            },
        )
        .await
        .unwrap();
    let hash = settings_hash(&project.settings.clone().validate_start().unwrap()).unwrap();
    let started = projects
        .start(
            &tenant,
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("host-start"),
                request_hash: start_request_hash(project.id, project.revision, &hash),
                settings_hash: hash,
                operation_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(tenant.operator_id, tenant.tenant_id, Some(project.id));
    let knowledge = Arc::new(MemoryKnowledgeRepository::default());
    let imported = knowledge
        .import_batch(
            &scope,
            vec![ImportItem {
                client_item_id: "public".into(),
                kind: SourceKind::Text,
                name: "public".into(),
                purpose: KnowledgePurpose::Public,
                text: Some(
                    (0..30)
                        .map(|ordinal| {
                            if ordinal == 29 {
                                format!(
                                    "{}Late documented detail.",
                                    "Long source context. ".repeat(90)
                                )
                            } else {
                                format!("Public description section {ordinal}.")
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n"),
                ),
                url: None,
                object_id: None,
                knowledge_release_id: None,
            }],
        )
        .await
        .unwrap();
    assert!(imported.items[0].release.is_some());
    let latest_project = projects.get(&scope, project.id).await.unwrap().unwrap();
    projects
        .update(
            &scope,
            project.id,
            latest_project.revision,
            ProjectPatch {
                document_scope: Some(DocumentScope::default()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(
        knowledge
            .get_document_manifest(&scope, started.document_manifest.manifest_id)
            .await
            .unwrap()
            .is_none()
    );
    let executor = Arc::new(AcceptedExecutor(AtomicUsize::new(0)));
    let model = Arc::new(GroundedModel(AtomicUsize::new(0), AtomicUsize::new(1)));
    let state = AppState::with_stores_and_auth_and_projects_and_knowledge(
        Arc::new(MemoryOperationStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
        Arc::new(MemoryAuthRepository::development_with_password("unused")),
        projects,
        knowledge.clone(),
        EventBus::default(),
        false,
    )
    .with_content_repository(Arc::new(MemoryContentRepository::default()));
    let host = RepositoryHostOps::new(knowledge).with_content(state.clone());
    assert_eq!(
        host.content_start(
            &scope,
            ContentStartRequest {
                cycle_id: Some(started.cycle_id)
            }
        )
        .await
        .unwrap_err()
        .code,
        HostOpErrorCode::CapabilityMissing
    );
    assert!(
        state
            .knowledge_repository()
            .get_document_manifest(&scope, started.document_manifest.manifest_id)
            .await
            .unwrap()
            .is_none(),
        "missing model must fail before planning changes state"
    );
    state.configure_content_model(model.clone());
    assert_eq!(
        host.content_start(&scope, ContentStartRequest { cycle_id: None })
            .await
            .unwrap_err()
            .code,
        HostOpErrorCode::CapabilityMissing
    );
    assert!(
        state
            .knowledge_repository()
            .get_document_manifest(&scope, started.document_manifest.manifest_id)
            .await
            .unwrap()
            .is_none(),
        "missing executor must fail before planning changes state"
    );
    state.configure_content_executor(executor.clone());
    let wrong_cycle = Uuid::new_v4();
    assert_eq!(
        host.content_start(
            &scope,
            ContentStartRequest {
                cycle_id: Some(wrong_cycle),
            }
        )
        .await
        .unwrap_err()
        .code,
        HostOpErrorCode::NotFound
    );
    let foreign = TenantScope::new(scope.operator_id, Uuid::new_v4().into(), scope.project_id);
    assert_eq!(
        host.content_start(
            &foreign,
            ContentStartRequest {
                cycle_id: Some(started.cycle_id),
            }
        )
        .await
        .unwrap_err()
        .code,
        HostOpErrorCode::NotFound
    );
    let started_ref = host
        .content_start(&scope, ContentStartRequest { cycle_id: None })
        .await
        .unwrap();
    assert_eq!(started_ref.coverage.total, 2);
    assert_eq!(executor.0.load(Ordering::SeqCst), 1);
    let sealed = state
        .knowledge_repository()
        .get_document_manifest(&scope, started.document_manifest.manifest_id)
        .await
        .unwrap()
        .unwrap();
    assert!(sealed.sealed);
    assert_eq!(sealed.expected_count, Some(2));
    let release_id = sealed.knowledge_release_id;
    let newer = state
        .knowledge_repository()
        .import_batch(
            &scope,
            vec![
                ImportItem {
                    client_item_id: "newer".into(),
                    kind: SourceKind::Text,
                    name: "newer".into(),
                    purpose: KnowledgePurpose::Public,
                    text: Some("Newer public description".into()),
                    url: None,
                    object_id: None,
                    knowledge_release_id: None,
                },
                ImportItem {
                    client_item_id: "internal".into(),
                    kind: SourceKind::Text,
                    name: "Internal fixture source".into(),
                    purpose: KnowledgePurpose::Internal,
                    text: Some("Internal fixture detail.".into()),
                    url: None,
                    object_id: None,
                    knowledge_release_id: None,
                },
            ],
        )
        .await
        .unwrap();
    assert_ne!(
        newer.items[0]
            .release
            .as_ref()
            .unwrap()
            .knowledge_release_id,
        release_id
    );
    assert_eq!(
        host.content_start(
            &scope,
            ContentStartRequest {
                cycle_id: Some(started.cycle_id),
            }
        )
        .await
        .unwrap(),
        started_ref,
        "sealed replay must retain the execution"
    );
    assert_eq!(
        state
            .knowledge_repository()
            .get_document_manifest(&scope, started.document_manifest.manifest_id)
            .await
            .unwrap()
            .unwrap()
            .knowledge_release_id,
        release_id,
        "newer knowledge must not overwrite the sealed cycle"
    );
    let read = host
        .content_execution_read(
            &scope,
            ContentExecutionReadRequest {
                execution_id: started_ref.execution_id,
            },
        )
        .await
        .unwrap();
    assert_eq!(read, started_ref);
    let first = host
        .content_items_read(
            &scope,
            ContentItemsReadRequest {
                execution_id: started_ref.execution_id,
                cursor: None,
                limit: Some(1),
            },
        )
        .await
        .unwrap();
    assert_eq!(first.total, 2);
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.items[0].automatic_repair_count, 0);
    let json = serde_json::to_string(&first).unwrap();
    for forbidden in [
        "\"quotes\":",
        "\"evidence\":",
        "\"brief\":",
        "\"document\":",
        "\"markdown\":",
        "\"provider\":",
        "\"token\":",
    ] {
        assert!(!json.contains(forbidden), "host page leaked {forbidden}");
    }
    let cursor = first.next_cursor.clone().unwrap();
    let second = host
        .content_items_read(
            &scope,
            ContentItemsReadRequest {
                execution_id: started_ref.execution_id,
                cursor: Some(cursor.clone()),
                limit: Some(1),
            },
        )
        .await
        .unwrap();
    assert_ne!(first.items[0].item_id, second.items[0].item_id);
    assert!(second.next_cursor.is_none());
    let tampered = format!("{cursor}x");
    assert_eq!(
        host.content_items_read(
            &scope,
            ContentItemsReadRequest {
                execution_id: started_ref.execution_id,
                cursor: Some(tampered),
                limit: Some(1)
            }
        )
        .await
        .unwrap_err()
        .code,
        HostOpErrorCode::InvalidRequest
    );
    assert_eq!(
        host.content_items_read(
            &foreign,
            ContentItemsReadRequest {
                execution_id: started_ref.execution_id,
                cursor: Some(cursor),
                limit: Some(1)
            }
        )
        .await
        .unwrap_err()
        .code,
        HostOpErrorCode::NotFound
    );
    assert_eq!(
        host.content_execution_read(
            &scope,
            ContentExecutionReadRequest {
                execution_id: Uuid::new_v4()
            }
        )
        .await
        .unwrap_err()
        .code,
        HostOpErrorCode::NotFound
    );
    assert_eq!(
        host.content_prepare(
            &scope,
            ContentPrepareRequest {
                execution_id: started_ref.execution_id,
                item_id: Uuid::new_v4(),
                evidence: vec![],
            }
        )
        .await
        .unwrap_err()
        .code,
        HostOpErrorCode::NotFound
    );
    assert_eq!(model.0.load(Ordering::SeqCst), 0);

    let item_id = first.items[0].item_id;
    let step = ContentStepRequest {
        execution_id: started_ref.execution_id,
        item_id,
    };
    assert_eq!(
        host.content_repair_begin(&foreign, step.clone())
            .await
            .unwrap_err()
            .code,
        HostOpErrorCode::NotFound,
        "the repair op must resolve scope before model work"
    );
    assert_eq!(
        host.content_repair_begin(&scope, step.clone())
            .await
            .unwrap_err()
            .code,
        HostOpErrorCode::Failed,
        "an unprepared item cannot be repaired"
    );
    assert_eq!(model.0.load(Ordering::SeqCst), 0);
    let source = &first.items[0].source_refs[0];
    let source_request = ContentSourceReadRequest {
        execution_id: step.execution_id,
        item_id: step.item_id,
        source_id: source.source_id,
        source_version_id: source.source_version_id,
        after_ordinal: None,
        limit: 24,
    };
    let page = host
        .content_source_read(&scope, source_request.clone())
        .await
        .unwrap();
    assert_eq!(page.chunks.len(), 24);
    assert_eq!(page.next_ordinal, Some(23));
    let late_page = host
        .content_source_read(
            &scope,
            ContentSourceReadRequest {
                after_ordinal: page.next_ordinal,
                ..source_request.clone()
            },
        )
        .await
        .unwrap();
    assert_eq!(late_page.chunks.len(), 6);
    assert_eq!(late_page.chunks[0].ordinal, 24);
    assert!(late_page.next_ordinal.is_none());
    let late_chunk = late_page.chunks.last().unwrap();
    assert!(late_chunk.text.find("Late documented detail.").unwrap() > 1600);
    let selected = ContentPrepareRequest {
        execution_id: step.execution_id,
        item_id: step.item_id,
        evidence: vec![ContentEvidence {
            reference: EvidenceRef {
                source_version_id: source.source_version_id,
                chunk_id: Some(late_chunk.chunk_id),
                locator: late_chunk.locator.clone(),
            },
            exact_quote: "Late documented detail.".into(),
        }],
    };
    assert_eq!(
        host.content_source_read(&foreign, source_request.clone())
            .await
            .unwrap_err()
            .code,
        HostOpErrorCode::NotFound
    );
    for invalid in [
        ContentSourceReadRequest {
            source_id: Uuid::new_v4(),
            ..source_request.clone()
        },
        ContentSourceReadRequest {
            source_version_id: Uuid::new_v4(),
            ..source_request.clone()
        },
        ContentSourceReadRequest {
            source_id: newer.items[1].source.as_ref().unwrap().source_id,
            ..source_request.clone()
        },
        ContentSourceReadRequest {
            source_id: newer.items[1].source.as_ref().unwrap().source_id,
            source_version_id: newer.items[1]
                .source_version
                .as_ref()
                .unwrap()
                .source_version_id,
            ..source_request.clone()
        },
    ] {
        assert_eq!(
            host.content_source_read(&scope, invalid)
                .await
                .unwrap_err()
                .code,
            HostOpErrorCode::Denied
        );
    }
    let mut fabricated_quote = selected.clone();
    fabricated_quote.evidence[0].exact_quote = "Unsupported fixture claim.".into();
    let mut wrong_chunk = selected.clone();
    wrong_chunk.evidence[0].reference.chunk_id = Some(Uuid::new_v4());
    let mut wrong_locator = selected.clone();
    wrong_locator.evidence[0].reference.locator = page.chunks[0].locator.clone();
    let mut private_evidence = selected.clone();
    let private_source = newer.items[1].source.as_ref().unwrap();
    let private_detail = state
        .knowledge_repository()
        .get_source_detail(&scope, private_source.source_id)
        .await
        .unwrap()
        .unwrap();
    let private_chunk = &private_detail.chunks[0];
    private_evidence.evidence[0] = ContentEvidence {
        reference: EvidenceRef {
            source_version_id: private_chunk.source_version_id,
            chunk_id: Some(private_chunk.chunk_id),
            locator: private_chunk.locator.clone(),
        },
        exact_quote: private_chunk.text.clone(),
    };
    for invalid in [
        fabricated_quote,
        wrong_chunk,
        wrong_locator,
        private_evidence,
    ] {
        assert_eq!(
            host.content_prepare(&scope, invalid)
                .await
                .unwrap_err()
                .code,
            HostOpErrorCode::Failed
        );
    }
    assert_eq!(model.0.load(Ordering::SeqCst), 0);
    let prepared = host
        .content_prepare(&scope, selected.clone())
        .await
        .unwrap();
    assert_eq!(prepared.status, ContentItemStatus::Prepared);
    let projected = serde_json::to_string(&prepared).unwrap();
    assert!(!projected.contains("Public description"));
    let drafted = generate_selected(&host, &scope, step.clone()).await;
    assert_eq!(drafted.status, ContentItemStatus::Drafted);
    let (checked, first_check) = check_selected(&host, &scope, step.clone()).await;
    assert_eq!(checked.status, ContentItemStatus::NeedsRepair);
    assert_eq!(
        host.content_check_finish(&foreign, first_check.clone())
            .await
            .unwrap_err()
            .code,
        HostOpErrorCode::NotFound,
        "a completed check token cannot bypass tenant scope"
    );
    let (repaired, repair_finish) = repair_selected(&host, &scope, step.clone()).await;
    assert_eq!(repaired.status, ContentItemStatus::Drafted);
    assert_eq!(repaired.automatic_repair_count, 1);
    assert_eq!(
        host.content_repair_finish(&foreign, repair_finish.clone())
            .await
            .unwrap_err()
            .code,
        HostOpErrorCode::NotFound,
        "a completed repair token cannot bypass tenant scope"
    );
    assert_eq!(
        host.content_repair_finish(
            &scope,
            ContentRepairFinishRequest {
                token: first_check.token,
                ..repair_finish.clone()
            }
        )
        .await
        .unwrap_err()
        .code,
        HostOpErrorCode::Failed,
        "a completed check token cannot authorize a repair completion"
    );
    let (ready, _) = check_selected(&host, &scope, step.clone()).await;
    assert_eq!(ready.status, ContentItemStatus::Ready);
    assert_eq!(
        host.content_check_finish(&scope, first_check)
            .await
            .unwrap(),
        ready
    );
    assert_eq!(
        host.content_repair_finish(&scope, repair_finish)
            .await
            .unwrap(),
        ready
    );
    let persisted = state
        .content_service()
        .repository()
        .get_item(&scope, step.execution_id, step.item_id)
        .await
        .unwrap()
        .unwrap();
    let revisions = state
        .content_service()
        .repository()
        .list_revisions(&scope, persisted.asset_id.unwrap())
        .await
        .unwrap();
    assert_eq!(
        revisions.len(),
        2,
        "completed repair replay cannot append a revision"
    );
    for revision in &revisions {
        assert_eq!(
            state
                .content_service()
                .repository()
                .list_checks(&scope, revision.revision_id)
                .await
                .unwrap()
                .len(),
            1,
            "completed check replay cannot append a check"
        );
    }
    let second_step = ContentStepRequest {
        execution_id: started_ref.execution_id,
        item_id: second.items[0].item_id,
    };
    host.content_prepare(&scope, selected_prepare(&host, &scope, &second_step).await)
        .await
        .unwrap();
    generate_selected(&host, &scope, second_step.clone()).await;
    check_selected(&host, &scope, second_step).await;
    assert_eq!(model.0.load(Ordering::SeqCst), 6);
    let handoff = host
        .content_close(
            &scope,
            ContentCloseRequest {
                execution_id: started_ref.execution_id,
            },
        )
        .await
        .unwrap();
    assert_eq!(handoff.total, 2);
    let first_cycle = state
        .project_repository()
        .get_current_cycle(&scope, project.id)
        .await
        .unwrap()
        .unwrap();
    let next = state
        .project_repository()
        .schedule_next_cycle(
            &scope,
            project.id,
            started.cycle_id,
            first_cycle.cutoff_at + chrono::Duration::days(1),
        )
        .await
        .unwrap();
    assert_eq!(
        host.content_start(
            &scope,
            ContentStartRequest {
                cycle_id: Some(started.cycle_id),
            }
        )
        .await
        .unwrap_err()
        .code,
        HostOpErrorCode::Failed,
        "an older cycle cannot be started or replanned"
    );
    let next_ref = host
        .content_start(
            &scope,
            ContentStartRequest {
                cycle_id: Some(next.cycle_id),
            },
        )
        .await
        .unwrap();
    assert_ne!(next_ref.execution_id, started_ref.execution_id);
    assert_eq!(next_ref.coverage.total, 2);
    assert_eq!(
        state
            .knowledge_repository()
            .get_document_manifest(&scope, next.document_manifest.as_ref().unwrap().manifest_id)
            .await
            .unwrap()
            .unwrap()
            .expected_count,
        Some(2),
        "successor planning uses frozen config, not the edited project draft"
    );
    let next_items = state
        .content_service()
        .repository()
        .list_items(&scope, next_ref.execution_id)
        .await
        .unwrap();
    let changed = host
        .content_prepare(
            &scope,
            selected_prepare(
                &host,
                &scope,
                &ContentStepRequest {
                    execution_id: next_ref.execution_id,
                    item_id: next_items[0].item_id,
                },
            )
            .await,
        )
        .await
        .unwrap();
    assert_eq!(changed.status, ContentItemStatus::Prepared);
    assert_eq!(
        model.0.load(Ordering::SeqCst),
        6,
        "a changed public source dependency must prepare a fresh draft"
    );
    let original_source = state
        .knowledge_repository()
        .get_source(&scope, source.source_id)
        .await
        .unwrap()
        .unwrap();
    state
        .knowledge_repository()
        .revise_source_text(
            &scope,
            original_source.source_id,
            original_source.revision,
            "supersede-fixture-source",
            ReviseSourceTextCommand {
                base_version_id: source.source_version_id,
                media_type: "text/plain".into(),
                text: "Replacement public description.".into(),
            },
        )
        .await
        .unwrap();
    assert!(
        host.content_source_read(&scope, source_request)
            .await
            .is_err(),
        "a superseded frozen source must not remain eligible for reading"
    );
    assert!(
        host.content_prepare(
            &scope,
            ContentPrepareRequest {
                execution_id: next_ref.execution_id,
                item_id: next_items[1].item_id,
                evidence: selected.evidence,
            }
        )
        .await
        .is_err(),
        "a superseded frozen source must not prepare another branch"
    );
}

#[tokio::test]
async fn two_automatic_content_cycles_reuse_the_same_checked_revision_without_model_calls() {
    let tenant = TenantScope::new(Uuid::new_v4().into(), Uuid::new_v4().into(), None);
    let projects = Arc::new(MemoryProjectRepository::default());
    let project = projects
        .create(
            &tenant,
            ProjectCreate {
                slug: None,
                display_name: "Cycle reuse".into(),
                settings: ProjectSettings {
                    brand_name: "Example".into(),
                    market: "US".into(),
                    language: "en".into(),
                    initial_sources: vec![InitialSource {
                        kind: InitialSourceKind::Text,
                        value: "Public description".into(),
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
    let accepted = projects
        .start(
            &tenant,
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("reuse-start"),
                request_hash: start_request_hash(project.id, project.revision, &hash),
                settings_hash: hash,
                operation_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(tenant.operator_id, tenant.tenant_id, Some(project.id));
    let knowledge = Arc::new(MemoryKnowledgeRepository::default());
    knowledge
        .import_batch(
            &scope,
            vec![ImportItem {
                client_item_id: "public".into(),
                kind: SourceKind::Text,
                name: "public".into(),
                purpose: KnowledgePurpose::Public,
                text: Some("Public description".into()),
                url: None,
                object_id: None,
                knowledge_release_id: None,
            }],
        )
        .await
        .unwrap();
    let model = Arc::new(GroundedModel(AtomicUsize::new(0), AtomicUsize::new(0)));
    let executor = Arc::new(AcceptedExecutor(AtomicUsize::new(0)));
    let repository = Arc::new(MemoryContentRepository::default());
    let state = AppState::with_stores_and_auth_and_projects_and_knowledge(
        Arc::new(MemoryOperationStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
        Arc::new(MemoryAuthRepository::development_with_password("unused")),
        projects.clone(),
        knowledge.clone(),
        EventBus::default(),
        false,
    )
    .with_content_repository(repository.clone());
    state.configure_content_model(model.clone());
    state.configure_content_executor(executor);
    let host = RepositoryHostOps::new(knowledge).with_content(state);
    let first = host
        .content_start(
            &scope,
            ContentStartRequest {
                cycle_id: Some(accepted.cycle_id),
            },
        )
        .await
        .unwrap();
    let first_item = repository
        .list_items(&scope, first.execution_id)
        .await
        .unwrap()
        .remove(0);
    let first_step = ContentStepRequest {
        execution_id: first.execution_id,
        item_id: first_item.item_id,
    };
    host.content_prepare(&scope, selected_prepare(&host, &scope, &first_step).await)
        .await
        .unwrap();
    generate_selected(&host, &scope, first_step.clone()).await;
    check_selected(&host, &scope, first_step).await;
    let first_checked = repository
        .get_item(&scope, first.execution_id, first_item.item_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_checked.status, ContentItemStatus::Ready);
    assert_eq!(model.0.load(Ordering::SeqCst), 2);
    host.content_close(
        &scope,
        ContentCloseRequest {
            execution_id: first.execution_id,
        },
    )
    .await
    .unwrap();
    let first_cycle = projects
        .get_current_cycle(&scope, project.id)
        .await
        .unwrap()
        .unwrap();
    let next = projects
        .schedule_next_cycle(
            &scope,
            project.id,
            accepted.cycle_id,
            first_cycle.cutoff_at + chrono::Duration::days(1),
        )
        .await
        .unwrap();
    let second = host
        .content_start(
            &scope,
            ContentStartRequest {
                cycle_id: Some(next.cycle_id),
            },
        )
        .await
        .unwrap();
    let second_item = repository
        .list_items(&scope, second.execution_id)
        .await
        .unwrap()
        .remove(0);
    assert_ne!(first.execution_id, second.execution_id);
    assert_ne!(first_item.item_id, second_item.item_id);
    let prepared = host
        .content_prepare(
            &scope,
            selected_prepare(
                &host,
                &scope,
                &ContentStepRequest {
                    execution_id: second.execution_id,
                    item_id: second_item.item_id,
                },
            )
            .await,
        )
        .await
        .unwrap();
    assert_eq!(prepared.status, ContentItemStatus::Ready);
    assert_eq!(model.0.load(Ordering::SeqCst), 2);
    let reused = repository
        .get_item(&scope, second.execution_id, second_item.item_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reused.ready_revision_id, first_checked.ready_revision_id);
    let binding = reused.reuse_binding.unwrap();
    assert_eq!(binding.origin_execution_id, first.execution_id);
    assert_eq!(binding.origin_item_id, first_item.item_id);
    assert_eq!(
        repository
            .resolve_checked_revision(
                &scope,
                second.execution_id,
                second_item.item_id,
                first_checked.ready_revision_id.unwrap(),
            )
            .await
            .unwrap()
            .unwrap()
            .revision_id,
        first_checked.ready_revision_id.unwrap()
    );
    let foreign = TenantScope::new(scope.operator_id, Uuid::new_v4().into(), scope.project_id);
    assert!(
        repository
            .resolve_checked_revision(
                &foreign,
                second.execution_id,
                second_item.item_id,
                first_checked.ready_revision_id.unwrap(),
            )
            .await
            .unwrap_or(None)
            .is_none(),
        "foreign tenants cannot resolve another tenant's origin"
    );
    host.content_close(
        &scope,
        ContentCloseRequest {
            execution_id: second.execution_id,
        },
    )
    .await
    .unwrap();
    assert_eq!(model.0.load(Ordering::SeqCst), 2);
}
