//! Explicit persistence suite: GEO_TEST_DATABASE_URL must name a disposable PostgreSQL database.
use chrono::Utc;
use geo_domain::{
    CodeBlockAttrs, ContentBlock, ContentBlockKind, ContentBrief, ContentEvidence, ContentFinding,
    ContentItemStatus, ContentRepository, ContentStep, DocumentManifestPlanRequest, ErrorCode,
    EvidenceRef, ImportItem, InitialSource, InitialSourceKind, InitialSourceVisibility,
    KnowledgePurpose, KnowledgeRepository, LinkAttrs, MediaReference, OrderedListAttrs,
    ProjectCreate, ProjectRepository, ProjectSettings, ProjectStartCommand,
    RICH_CHECK_POLICY_VERSION, RICH_REPAIR_POLICY_VERSION, RichContent, RichMark, RichNode,
    SourceKind, StructuredDocument, TableCellAttrs, TenantScope, hash_idempotency_key,
    settings_hash, start_request_hash,
};
use geo_persistence::{
    Database, DatabaseConfig, PgContentRepository, PgKnowledgeRepository, PgProjectRepository,
};
use uuid::Uuid;

fn text(value: &str, marks: Vec<RichMark>) -> RichNode {
    RichNode::Text {
        text: value.into(),
        marks,
    }
}

fn paragraph(value: &str) -> RichNode {
    RichNode::Paragraph {
        content: vec![text(value, vec![])],
    }
}

fn rich_block(node: RichNode, citation_id: Uuid) -> ContentBlock {
    ContentBlock {
        block_id: Uuid::new_v4(),
        kind: ContentBlockKind::Rich,
        text: String::new(),
        citation_ids: vec![citation_id],
        items: vec![],
        rich: Some(RichContent { version: 1, node }),
    }
}

fn rich_document(citation_id: Uuid) -> StructuredDocument {
    StructuredDocument {
        title: "Evidence-backed format".into(),
        schema_version: Some(2),
        blocks: vec![
            rich_block(
                RichNode::Paragraph {
                    content: vec![
                        text("Supported ", vec![]),
                        text("claim", vec![RichMark::Bold]),
                        text(
                            " link",
                            vec![RichMark::Link {
                                attrs: LinkAttrs {
                                    href: "https://example.org/docs".into(),
                                    title: Some("Reference".into()),
                                },
                            }],
                        ),
                    ],
                },
                citation_id,
            ),
            rich_block(
                RichNode::Table {
                    content: vec![RichNode::TableRow {
                        content: vec![
                            RichNode::TableHeader {
                                attrs: Some(TableCellAttrs {
                                    colspan: 1,
                                    rowspan: 1,
                                    colwidth: None,
                                    alignment: None,
                                }),
                                content: vec![paragraph("Capability")],
                            },
                            RichNode::TableCell {
                                attrs: None,
                                content: vec![paragraph("Documented")],
                            },
                        ],
                    }],
                },
                citation_id,
            ),
            rich_block(
                RichNode::CodeBlock {
                    attrs: Some(CodeBlockAttrs {
                        language: Some("json".into()),
                    }),
                    content: vec![text("{\"enabled\": true}", vec![])],
                },
                citation_id,
            ),
            rich_block(
                RichNode::OrderedList {
                    attrs: OrderedListAttrs { start: 3 },
                    content: vec![RichNode::ListItem {
                        content: vec![
                            paragraph("First item"),
                            RichNode::BulletList {
                                content: vec![RichNode::ListItem {
                                    content: vec![paragraph("Nested item")],
                                }],
                            },
                        ],
                    }],
                },
                citation_id,
            ),
        ],
    }
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn rich_edit_survives_reconnect_and_versioned_repair_preserves_history() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable test database URL");
    let config = DatabaseConfig::from_url(url).unwrap();
    let database = Database::connect_and_migrate(&config).await.unwrap();
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'Test')")
        .bind(operator)
        .bind(format!("rich-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,'Test')",
    )
    .bind(tenant)
    .bind(operator)
    .bind(format!("rich-{tenant}"))
    .execute(database.pool())
    .await
    .unwrap();
    let tenant_scope = TenantScope::new(operator.into(), tenant.into(), None);
    let projects = PgProjectRepository::from_database(&database);
    let project = projects
        .create(
            &tenant_scope,
            ProjectCreate {
                slug: None,
                display_name: "Synthetic content project".into(),
                settings: ProjectSettings {
                    brand_name: "Example".into(),
                    market: "global".into(),
                    language: "en".into(),
                    initial_sources: vec![InitialSource {
                        kind: InitialSourceKind::Text,
                        value: "Synthetic public reference".into(),
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
    let frozen = project.settings.clone().validate_start().unwrap();
    let frozen_hash = settings_hash(&frozen).unwrap();
    let started = projects
        .start(
            &tenant_scope,
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("rich-content-test"),
                request_hash: start_request_hash(project.id, project.revision, &frozen_hash),
                settings_hash: frozen_hash,
                operation_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.id));
    let knowledge = PgKnowledgeRepository::from_database(&database);
    let imported = knowledge
        .import_batch(
            &scope,
            vec![ImportItem {
                client_item_id: format!("rich-{tenant}"),
                kind: SourceKind::Text,
                name: "Public synthetic source".into(),
                purpose: KnowledgePurpose::Public,
                text: Some("Documented capability for an example product.".into()),
                url: None,
                object_id: None,
                knowledge_release_id: None,
            }],
        )
        .await
        .unwrap();
    let source_id = imported.items[0].source.as_ref().unwrap().source_id;
    let release_id = imported.items[0]
        .release
        .as_ref()
        .unwrap()
        .knowledge_release_id;
    let detail = knowledge
        .get_source_detail(&scope, source_id)
        .await
        .unwrap()
        .unwrap();
    let chunk = &detail.chunks[0];
    let citation_id = chunk.chunk_id;
    let reference = EvidenceRef {
        source_version_id: chunk.source_version_id,
        chunk_id: Some(citation_id),
        locator: chunk.locator.clone(),
    };
    let quote = ContentEvidence {
        reference: reference.clone(),
        exact_quote: chunk.text.clone(),
    };
    let mut document_scope = frozen.document_scope.clone();
    document_scope.markets = frozen.effective_markets();
    document_scope.languages = frozen.effective_languages();
    let manifest = knowledge
        .plan_document_manifest(
            &scope,
            DocumentManifestPlanRequest {
                manifest_id: started.document_manifest.manifest_id,
                knowledge_release_id: release_id,
            },
            document_scope,
        )
        .await
        .unwrap();
    let item_id = manifest
        .items
        .iter()
        .find(|item| item.source_version_refs.contains(&chunk.source_version_id))
        .expect("source-backed manifest item")
        .document_manifest_item_id;
    let repo = PgContentRepository::from_database(&database);
    let execution = repo
        .start(&scope, started.cycle_id, manifest, "rich-test-policy")
        .await
        .unwrap();
    let prepare = repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Prepare,
            "test",
            Utc::now(),
            600,
        )
        .await
        .unwrap();
    repo.complete_prepare(
        &scope,
        &prepare,
        ContentBrief {
            brief_id: Uuid::new_v4(),
            title: "Evidence-backed format".into(),
            objective: "Cited synthetic example".into(),
            evidence: vec![reference.clone()],
            quotes: vec![quote.clone()],
            created_at: Utc::now(),
        },
    )
    .await
    .unwrap();
    let generate = repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Generate,
            "test",
            Utc::now(),
            600,
        )
        .await
        .unwrap();
    let old_document = StructuredDocument {
        title: "Original *title*".into(),
        blocks: vec![ContentBlock {
            block_id: Uuid::new_v4(),
            kind: ContentBlockKind::Paragraph,
            text: "Original literal *claim*.".into(),
            citation_ids: vec![citation_id],
            items: vec![],
            rich: None,
        }],
        schema_version: None,
    };
    let original = repo
        .complete_generate(&scope, &generate, old_document.clone())
        .await
        .unwrap();
    assert_eq!(
        original.markdown,
        "# Original *title*\n\nOriginal literal *claim*."
    );
    let old_check = repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Check,
            "test",
            Utc::now(),
            600,
        )
        .await
        .unwrap();
    repo.complete_check(&scope, &old_check, vec![])
        .await
        .unwrap();

    let document = rich_document(citation_id);
    let edited = repo
        .edit(
            &scope,
            original.asset_id,
            original.revision_id,
            document.clone(),
        )
        .await
        .unwrap();
    assert_eq!(edited.revision, 2);
    assert_eq!(edited.base_revision_id, Some(original.revision_id));
    assert_eq!(edited.evidence, vec![reference.clone()]);
    assert_eq!(edited.quotes, vec![quote.clone()]);
    assert_eq!(edited.document, document);
    assert_eq!(edited.markdown, document.markdown());
    assert!(edited.markdown.contains("<table>"));
    assert!(edited.markdown.contains("```json"));
    assert!(edited.markdown.contains("3. "));
    assert!(edited.markdown.contains("<strong>claim</strong>"));
    assert!(document.html(&edited.evidence).unwrap().contains("<table>"));
    let sections = document.check_sections();
    assert_eq!(
        sections.len(),
        5,
        "title and every top-level block must be checked"
    );
    assert!(sections[1].1.contains("Supported claim link"));
    assert!(sections[2].1.contains("Documented"));
    assert!(sections[3].1.contains("enabled"));
    assert!(sections[4].1.contains("Nested item"));

    let stored: serde_json::Value =
        sqlx::query_scalar("SELECT body FROM content_revisions WHERE revision_id=$1")
            .bind(edited.revision_id)
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert_eq!(stored["document"], serde_json::to_value(&document).unwrap());
    assert_eq!(stored["revision_id"], serde_json::json!(edited.revision_id));
    assert_eq!(
        stored["evidence"],
        serde_json::to_value(&edited.evidence).unwrap()
    );
    let reconnected = Database::connect_and_migrate(&config).await.unwrap();
    let loaded_repo = PgContentRepository::from_database(&reconnected);
    let history = loaded_repo
        .list_revisions(&scope, original.asset_id)
        .await
        .unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].revision_id, original.revision_id);
    assert_eq!(history[0].document, old_document);
    assert_eq!(history[0].markdown, original.markdown);
    assert_eq!(history[1], edited);

    assert_eq!(
        loaded_repo
            .claim(
                &scope,
                execution.execution_id,
                item_id,
                ContentStep::Check,
                "legacy",
                Utc::now(),
                600
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict,
        "v2 must not enter a legacy checker"
    );
    let check = loaded_repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Check,
            RICH_CHECK_POLICY_VERSION,
            Utc::now(),
            600,
        )
        .await
        .unwrap();
    let finding = ContentFinding {
        finding_id: Uuid::new_v4(),
        code: "unverified_claim".into(),
        block_id: Some(document.blocks[0].block_id),
        evidence: vec![reference.clone()],
        detail: "Revise the indicated claim".into(),
        blocking: true,
    };
    let checked = loaded_repo
        .complete_check(&scope, &check, vec![finding.clone()])
        .await
        .unwrap();
    assert_eq!(checked.status, ContentItemStatus::NeedsRepair);
    assert_eq!(
        loaded_repo
            .list_checks(&scope, edited.revision_id)
            .await
            .unwrap()[0]
            .findings,
        vec![finding]
    );
    assert_eq!(
        loaded_repo
            .claim(
                &scope,
                execution.execution_id,
                item_id,
                ContentStep::Repair,
                "legacy",
                Utc::now(),
                600
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let repair = loaded_repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Repair,
            RICH_REPAIR_POLICY_VERSION,
            Utc::now(),
            600,
        )
        .await
        .unwrap();
    let mut repaired_document = document.clone();
    if let Some(RichContent {
        node: RichNode::Paragraph { content },
        ..
    }) = &mut repaired_document.blocks[0].rich
    {
        content[1] = text("verified claim", vec![RichMark::Bold]);
    } else {
        panic!("first block is the flagged paragraph");
    }
    let mut illicit = repaired_document.clone();
    illicit.blocks[1].block_id = Uuid::new_v4();
    assert_eq!(
        loaded_repo
            .complete_repair(&scope, &repair, illicit)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest,
        "repair cannot alter an unaffected table's identity"
    );
    let mut downgrade = old_document.clone();
    downgrade.title = document.title.clone();
    assert_eq!(
        loaded_repo
            .complete_repair(&scope, &repair, downgrade)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict,
        "automatic repair must not replace v2 with legacy content"
    );
    assert_eq!(
        loaded_repo
            .list_revisions(&scope, original.asset_id)
            .await
            .unwrap()
            .len(),
        2
    );
    let repaired = loaded_repo
        .complete_repair(&scope, &repair, repaired_document.clone())
        .await
        .unwrap();
    assert_eq!(repaired.document, repaired_document);
    assert_eq!(repaired.document.blocks[1..], document.blocks[1..]);
    assert_eq!(repaired.base_revision_id, Some(edited.revision_id));
    assert_eq!(repaired.evidence, edited.evidence);
    assert_eq!(repaired.quotes, edited.quotes);
    assert_eq!(repaired.markdown, repaired_document.markdown());
    let recheck = loaded_repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Check,
            RICH_CHECK_POLICY_VERSION,
            Utc::now(),
            600,
        )
        .await
        .unwrap();
    assert_eq!(
        loaded_repo
            .complete_check(&scope, &recheck, vec![])
            .await
            .unwrap()
            .status,
        ContentItemStatus::Ready
    );
    let final_history = loaded_repo
        .list_revisions(&scope, original.asset_id)
        .await
        .unwrap();
    assert_eq!(final_history.len(), 3);
    assert_eq!(final_history[0].markdown, original.markdown);
    assert_eq!(final_history[1].document, document);
    assert_eq!(final_history[2].document, repaired_document);
    assert!(
        loaded_repo
            .list_checks(&scope, edited.revision_id)
            .await
            .unwrap()[0]
            .findings
            .iter()
            .any(|f| f.blocking)
    );

    let foreign_project = projects
        .create(
            &tenant_scope,
            ProjectCreate {
                slug: None,
                display_name: "Unrelated synthetic project".into(),
                settings: ProjectSettings {
                    brand_name: "Example".into(),
                    market: "global".into(),
                    language: "en".into(),
                    ..ProjectSettings::default()
                },
            },
        )
        .await
        .unwrap();
    let foreign_scope = TenantScope::new(operator.into(), tenant.into(), Some(foreign_project.id));
    assert!(
        loaded_repo
            .list_revisions(&foreign_scope, original.asset_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        loaded_repo
            .list_revisions(
                &TenantScope::new(operator.into(), Uuid::new_v4().into(), Some(project.id)),
                original.asset_id
            )
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        loaded_repo
            .edit(
                &foreign_scope,
                original.asset_id,
                repaired.revision_id,
                repaired_document.clone()
            )
            .await
            .is_err()
    );

    let mut media_document = repaired_document.clone();
    media_document.blocks.push(rich_block(
        RichNode::Media {
            attrs: MediaReference {
                object_id: Uuid::new_v4(),
                object_version: 1,
                sha256: "a".repeat(64),
                alt: "Synthetic visual".into(),
                caption: "No authorization binding".into(),
            },
        },
        citation_id,
    ));
    assert_eq!(
        loaded_repo
            .edit(
                &scope,
                original.asset_id,
                repaired.revision_id,
                media_document
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest,
        "unbound media cannot be committed"
    );
    assert_eq!(
        loaded_repo
            .list_revisions(&scope, original.asset_id)
            .await
            .unwrap()
            .len(),
        3
    );
}
