use chrono::Utc;
use geo_domain::{
    CHANNEL_VARIANT_POLICY, ContentBlock, ContentBlockKind, ContentMediaBinding,
    ContentMediaBindingState, ContentRevision, DistributionRepository, ErrorCode, MediaObjectKey,
    MemoryDistributionRepository, OperatorId, PlatformPlacement, ProjectId,
    RICH_CHANNEL_VARIANT_POLICY, RICH_DISTRIBUTION_FORMAT, RICH_MARKDOWN_FORMAT,
    StructuredDocument, TenantId, TenantScope, VerifiedImage, prepare_rich_variant,
    prepare_rich_variant_authorized, prepare_variant, rich_publication_structured_sha256,
    validate_rich_publication_payload,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use uuid::Uuid;

fn rich(node: Value) -> StructuredDocument {
    serde_json::from_value(json!({
        "title": "A <title>",
        "schema_version": 2,
        "blocks": [{
            "block_id": Uuid::nil(),
            "kind": "rich",
            "text": "",
            "items": [],
            "citation_ids": [],
            "rich": {"version": 1, "node": node}
        }]
    }))
    .unwrap()
}
fn valid(node: Value) -> StructuredDocument {
    let result = rich(node);
    result.validate(&[]).unwrap();
    result
}
fn text(value: &str) -> Value {
    json!({"type":"text","text":value})
}
fn paragraph(value: &str) -> Value {
    json!({"type":"paragraph","content":[text(value)]})
}
fn revision(doc: StructuredDocument) -> ContentRevision {
    ContentRevision {
        revision_id: Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
        asset_id: Uuid::new_v4(),
        revision: 1,
        base_revision_id: None,
        derived_from_revision_id: None,
        markdown: doc.markdown(),
        document: doc,
        evidence: vec![],
        quotes: vec![],
        findings: vec![],
        created_at: Utc::now(),
    }
}
fn placement(formats: Vec<&str>) -> PlatformPlacement {
    PlatformPlacement {
        platform_id: "test".into(),
        placement_slot: "main".into(),
        capability_version: "1".into(),
        supported_formats: formats.into_iter().map(str::to_owned).collect(),
        unavailable_reason: None,
        fixture: true,
    }
}
#[test]
fn legacy_serialization_markdown_and_variant_identity_are_frozen() {
    let block_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    let raw = json!({"title":"Old *title*","blocks":[
        {"block_id":block_id,"kind":"heading","text":"A&B","citation_ids":[],"items":[]},
        {"block_id":Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),"kind":"list","text":"Lead","citation_ids":[],"items":["one","two"]}
    ]});
    let document: StructuredDocument = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(serde_json::to_value(&document).unwrap(), raw);
    // Byte-level baseline from pre-v2 HEAD `StructuredDocument` and
    // `ContentBlock` field order, including omitted optional fields.
    assert_eq!(
        serde_json::to_string(&document).unwrap(),
        "{\"title\":\"Old *title*\",\"blocks\":[{\"block_id\":\"11111111-1111-1111-1111-111111111111\",\"kind\":\"heading\",\"text\":\"A&B\",\"citation_ids\":[],\"items\":[]},{\"block_id\":\"22222222-2222-2222-2222-222222222222\",\"kind\":\"list\",\"text\":\"Lead\",\"citation_ids\":[],\"items\":[\"one\",\"two\"]}]}"
    );
    assert_eq!(
        document.markdown(),
        "# Old *title*\n\n## A&B\n\nLead\n\n- one\n- two"
    );
    assert_eq!(
        document.html(&[]).unwrap(),
        "<h1>Old *title*</h1><h2>A&amp;B</h2><p>Lead</p><ul><li><p>one</p></li><li><p>two</p></li></ul>"
    );
    let revision = revision(document);
    let variant = prepare_variant(&revision, &placement(vec!["faq"])).unwrap();
    assert_eq!(variant.policy_version, CHANNEL_VARIANT_POLICY);
    assert_eq!(variant.markdown, revision.markdown);
    // Independently derived from pre-v2 HEAD `distribution.rs` digest/identity:
    // SHA-256 of length-prefixed old title/Markdown, then frozen revision ID,
    // placement, policy and hash, with UUID v5 bits.
    assert_eq!(
        variant.payload_hash,
        "11e0eb7085fd3153b55342394a3398a48d18c6e44814e336631b9147fbe2d0b7"
    );
    assert_eq!(
        variant.variant_id.to_string(),
        "c6afef61-ae57-5843-bae4-6e02accf4d24"
    );
    assert!(variant.rich_payload.is_none());
    let saved = serde_json::to_value(&variant).unwrap();
    assert!(saved.get("rich_payload").is_none());
    assert_eq!(
        serde_json::from_value::<geo_domain::ChannelVariant>(saved).unwrap(),
        variant
    );
}

fn media_scope() -> TenantScope {
    TenantScope::new(
        OperatorId::new(Uuid::from_u128(11)),
        TenantId::new(Uuid::from_u128(12)),
        Some(ProjectId::new(Uuid::from_u128(13))),
    )
}

fn media_binding(scope: &TenantScope, object: Uuid, sha256: String) -> ContentMediaBinding {
    ContentMediaBinding {
        binding_id: Uuid::from_u128(14),
        operator_id: scope.operator_id,
        tenant_id: scope.tenant_id,
        project_id: scope.project_id.unwrap(),
        image: VerifiedImage {
            key: MediaObjectKey {
                object_id: object,
                object_version: 1,
                sha256,
            },
            media_type: "image/png".into(),
            byte_len: 160,
            width: 8,
            height: 7,
        },
        state: ContentMediaBindingState::Active,
        created_at: Utc::now(),
        withdrawn_at: None,
    }
}

#[test]
fn rich_identity_binds_tree_and_exact_ordered_verified_images() {
    let scope = media_scope();
    let object = Uuid::from_u128(21);
    let mut document = valid(json!({"type":"media","attrs":{
        "object_id":object,"object_version":1,"sha256":"a".repeat(64),
        "alt":"Detail","caption":"Image one"
    }}));
    let original_revision = revision(document.clone());
    let binding = media_binding(&scope, object, "a".repeat(64));
    let placement = placement(vec![RICH_MARKDOWN_FORMAT]);
    let original = prepare_rich_variant_authorized(
        &original_revision,
        &placement,
        &scope,
        std::slice::from_ref(&binding),
    )
    .unwrap();
    validate_rich_publication_payload(&original).unwrap();
    let payload = original.rich_payload.as_ref().unwrap();
    assert_eq!(payload.media.len(), 1);
    assert_eq!(payload.media[0].object, binding.image.key);
    assert_eq!(payload.media[0].binding_id, binding.binding_id);
    assert_eq!(payload.media[0].width, 8);
    assert_eq!(payload.media[0].height, 7);
    assert_eq!(
        payload.media[0].role,
        geo_domain::PublicationMediaRole::Image
    );
    assert_eq!(
        serde_json::from_value::<geo_domain::ChannelVariant>(
            serde_json::to_value(&original).unwrap()
        )
        .unwrap(),
        original
    );
    assert_eq!(
        rich_publication_structured_sha256(payload).unwrap(),
        "8a715d3f4176af43d5c3377764c0371985755bdac71dd96be901e6bd4316016e"
    );
    assert_eq!(
        original.payload_hash,
        "79ea2c42c4bfec9550ef7356c9a475686941bad1f8c676520e5577007d7f4874"
    );
    let generated = geo_domain::ChannelTargetInput::GeneratedPublish {
        content_revision_id: original.content_revision_id,
        variant_id: original.variant_id,
        publication_intent_id: Uuid::from_u128(31),
        distribution_target_id: Uuid::from_u128(32),
        origin_request_id: None,
        platform: original.platform_id.clone(),
        account_id: Uuid::from_u128(33),
        title: original.title.clone(),
        body: original.markdown.clone(),
        body_sha256: "c".repeat(64),
        payload_hash: original.payload_hash.clone(),
        evidence: vec![],
        rich_payload: original.rich_payload.clone(),
    };
    assert_eq!(
        serde_json::from_value::<geo_domain::ChannelTargetInput>(
            serde_json::to_value(&generated).unwrap()
        )
        .unwrap(),
        generated
    );
    let mut legacy_input = generated.clone();
    if let geo_domain::ChannelTargetInput::GeneratedPublish { rich_payload, .. } = &mut legacy_input
    {
        *rich_payload = None;
    }
    let legacy_json = serde_json::to_value(&legacy_input).unwrap();
    assert!(legacy_json.get("rich_payload").is_none());
    assert_eq!(
        serde_json::from_value::<geo_domain::ChannelTargetInput>(legacy_json).unwrap(),
        legacy_input
    );

    // The Markdown path has only object/version; changed bytes and digest
    // cannot replay the old variant even if rendered Markdown is identical.
    if let geo_domain::RichNode::Media { attrs } =
        &mut document.blocks[0].rich.as_mut().unwrap().node
    {
        attrs.sha256 = "b".repeat(64);
    }
    let changed_revision = revision(document);
    assert_eq!(original_revision.markdown, changed_revision.markdown);
    let changed = prepare_rich_variant_authorized(
        &changed_revision,
        &placement,
        &scope,
        &[media_binding(&scope, object, "b".repeat(64))],
    )
    .unwrap();
    assert_ne!(original.payload_hash, changed.payload_hash);
    assert_ne!(original.variant_id, changed.variant_id);
    let mut altered_manifest = original.clone();
    altered_manifest.rich_payload.as_mut().unwrap().media[0].width = 9;
    assert!(validate_rich_publication_payload(&altered_manifest).is_err());

    let mut unsplit = revision(valid(paragraph("same")));
    let mut split = unsplit.clone();
    split.document.blocks[0].rich.as_mut().unwrap().node =
        serde_json::from_value(json!({"type":"paragraph","content":[text("sa"),text("me")]}))
            .unwrap();
    split.markdown = split.document.markdown();
    assert_eq!(unsplit.markdown, split.markdown);
    let first = prepare_rich_variant(&unsplit, &placement).unwrap();
    let second = prepare_rich_variant(&split, &placement).unwrap();
    assert_ne!(first.payload_hash, second.payload_hash);
    assert_ne!(first.variant_id, second.variant_id);
    unsplit.markdown.push_str("changed");
    assert!(prepare_rich_variant(&unsplit, &placement).is_err());
}

#[test]
fn rich_media_requires_exact_active_scoped_binding_and_strict_payload() {
    let scope = media_scope();
    let object = Uuid::from_u128(21);
    let revision = revision(valid(json!({"type":"media","attrs":{
        "object_id":object,"object_version":1,"sha256":"a".repeat(64),
        "alt":"Detail","caption":"Image one"
    }})));
    let placement = placement(vec![RICH_MARKDOWN_FORMAT]);
    let binding = media_binding(&scope, object, "a".repeat(64));
    for bindings in [
        vec![],
        vec![media_binding(&scope, object, "b".repeat(64))],
        vec![ContentMediaBinding {
            tenant_id: TenantId::new(Uuid::new_v4()),
            ..binding.clone()
        }],
        vec![ContentMediaBinding {
            state: ContentMediaBindingState::Withdrawn,
            ..binding.clone()
        }],
        vec![ContentMediaBinding {
            image: VerifiedImage {
                width: 0,
                ..binding.image.clone()
            },
            ..binding.clone()
        }],
        vec![ContentMediaBinding {
            image: VerifiedImage {
                media_type: "image/svg+xml".into(),
                ..binding.image.clone()
            },
            ..binding.clone()
        }],
        vec![binding.clone(), binding.clone()],
    ] {
        assert!(prepare_rich_variant_authorized(&revision, &placement, &scope, &bindings).is_err());
    }
    let variant =
        prepare_rich_variant_authorized(&revision, &placement, &scope, &[binding]).unwrap();
    let mut payload = serde_json::to_value(variant.rich_payload.unwrap()).unwrap();
    payload["media"][0]["unknown"] = json!(true);
    assert!(serde_json::from_value::<geo_domain::RichPublicationPayload>(payload).is_err());
}

#[tokio::test]
async fn rich_single_request_reuses_original_intent_and_outbox() {
    let scope = media_scope();
    let object = Uuid::from_u128(21);
    let mut revision = revision(valid(json!({"type":"media","attrs":{
        "object_id":object,"object_version":1,"sha256":"a".repeat(64),
        "alt":"Detail","caption":"Image one"
    }})));
    revision.evidence.push(geo_domain::EvidenceRef {
        source_version_id: Uuid::from_u128(50),
        chunk_id: None,
        locator: geo_domain::ChunkLocator::Text {
            start_line: 1,
            end_line: 1,
            start_char: 0,
            end_char: 1,
        },
    });
    let request = geo_domain::ContentDistributionRequest {
        request_id: Uuid::from_u128(51),
        scope: scope.clone(),
        schema_version: 1,
        content_revision_id: revision.revision_id,
        content_asset_id: revision.asset_id,
        platform_id: "test".into(),
        placement_slot: "main".into(),
        account_id: Uuid::from_u128(52),
        account_owner_kind: "customer".into(),
        format: RICH_DISTRIBUTION_FORMAT.into(),
        idempotency_key_hash: "key".into(),
        request_hash: "request".into(),
        publication_intent_id: None,
        materialization_deferral: None,
        created_at: Utc::now(),
    };
    let binding = media_binding(&scope, object, "a".repeat(64));
    let repository = MemoryDistributionRepository::new();
    let intent = repository
        .materialize_rich_request_origin(&scope, &request, &revision, vec![binding.clone()])
        .await
        .unwrap();
    let bundle = repository
        .get_publication_bundle(&scope, intent.intent_id)
        .await
        .unwrap();
    bundle.validate_origin().unwrap();
    assert_eq!(
        bundle.variant.rich_payload.as_ref().unwrap().media[0].binding_id,
        binding.binding_id
    );
    let mut next = request;
    next.request_id = Uuid::from_u128(53);
    let reused = repository
        .materialize_rich_request_origin(&scope, &next, &revision, vec![binding])
        .await
        .unwrap();
    assert_eq!(intent.intent_id, reused.intent_id);
    assert_eq!(repository.publication_commands(&scope).await.len(), 1);
    let still_original = repository
        .get_publication_bundle(&scope, intent.intent_id)
        .await
        .unwrap();
    assert!(
        matches!(still_original.origin, geo_domain::PublicationOrigin::ContentRequest { request }
        if request.request_id == Uuid::from_u128(51))
    );
}
#[test]
fn rich_marks_lists_code_and_html_are_round_tripped_without_legacy_text() {
    let document = valid(json!({"type":"bulletList","content":[
        {"type":"listItem","content":[paragraph("first"),{"type":"orderedList","attrs":{"start":3},"content":[
            {"type":"listItem","content":[{"type":"paragraph","content":[
                {"type":"text","text":"A&B","marks":[{"type":"bold"},{"type":"link","attrs":{"href":"https://example.org/?a=1&b=2"}}]},
                {"type":"hardBreak"},{"type":"text","text":"code ` here","marks":[{"type":"code"}]}
            ]}]}
        ]}]},
        {"type":"listItem","content":[paragraph("last")]}
    ]}));
    assert_eq!(
        serde_json::from_value::<StructuredDocument>(serde_json::to_value(&document).unwrap())
            .unwrap(),
        document
    );
    assert!(
        document
            .markdown()
            .contains("3. [**A&amp;B**](https://example.org/?a=1&b=2)")
    );
    assert!(document.html(&[]).unwrap().contains("<ol start=\"3\">"));
    assert!(
        document
            .html(&[])
            .unwrap()
            .contains("href=\"https://example.org/?a=1&amp;b=2\"")
    );
    assert!(document.blocks[0].text.is_empty());
    assert!(document.blocks[0].items.is_empty());
    let code = valid(
        json!({"type":"codeBlock","attrs":{"language":"rust"},"content":[text("x < y\n```")]}),
    );
    assert!(code.markdown().contains("````rust\nx < y\n```\n````"));
    assert!(code.html(&[]).unwrap().contains("x &lt; y"));
    for level in 1..=6 {
        let doc =
            valid(json!({"type":"heading","attrs":{"level":level},"content":[text("Safe")] }));
        assert!(
            doc.html(&[])
                .unwrap()
                .contains(&format!("<h{level}>Safe</h{level}>"))
        );
    }
}
#[test]
fn table_spans_preserve_structure_in_markdown_html_and_reject_gaps() {
    let document = valid(json!({"type":"table","content":[
        {"type":"tableRow","content":[
            {"type":"tableHeader","attrs":{"colspan":2,"rowspan":1,"colwidth":[80,90],"textAlign":"center"},"content":[paragraph("head")]}
        ]},
        {"type":"tableRow","content":[
            {"type":"tableCell","content":[paragraph("a")]},
            {"type":"tableCell","content":[paragraph("b")]}
        ]}
    ]}));
    assert!(document.markdown().contains("<table><tbody>"));
    assert!(document.markdown().contains("colspan=\"2\""));
    assert!(
        document
            .html(&[])
            .unwrap()
            .contains("data-colwidth=\"80,90\"")
    );
    let invalid_table = rich(json!({"type":"table","content":[
        {"type":"tableRow","content":[{"type":"tableCell","content":[paragraph("a")]},{"type":"tableCell","content":[paragraph("b")]}]},
        {"type":"tableRow","content":[{"type":"tableCell","content":[paragraph("missing")]}]}
    ]}));
    assert_eq!(
        invalid_table.validate(&[]).unwrap_err().code,
        ErrorCode::InvalidRequest
    );
}
#[test]
fn unsafe_and_unknown_content_is_rejected_instead_of_flattened() {
    for node in [
        json!({"type":"iframe","attrs":{"src":"https://example.org"}}),
        json!({"type":"paragraph","attrs":{"geoCitations":[Uuid::new_v4()]},"content":[text("paste")]}),
        json!({"type":"paragraph","content":[{"type":"text","text":"bad","marks":[{"type":"bold","attrs":{"color":"red"}}]}]}),
        json!({"type":"paragraph","content":[{"type":"hardBreak","attrs":{"src":"unsafe"}}]}),
        json!({"type":"paragraph","content":[{"type":"text","text":"click","marks":[{"type":"link","attrs":{"href":"javascript:alert(1)"}}]}]}),
        json!({"type":"paragraph","content":[{"type":"text","text":"click","marks":[{"type":"link","attrs":{"href":"java\nscript:alert(1)"}}]}]}),
        json!({"type":"paragraph","content":[{"type":"text","text":"click","marks":[{"type":"link","attrs":{"href":"JaVaScRiPt:alert(1)"}}]}]}),
        json!({"type":"paragraph","content":[{"type":"text","text":"click","marks":[{"type":"link","attrs":{"href":"//unverified.example/path"}}]}]}),
        json!({"type":"paragraph","content":[{"type":"text","text":"click","marks":[{"type":"link","attrs":{"href":"data:text/html,unsafe"}}]}]}),
        json!({"type":"heading","attrs":{"level":7},"content":[text("bad")]}),
        json!({"type":"paragraph","content":[{"type":"hardBreak"},{"type":"table","content":[]}]}),
    ] {
        match serde_json::from_value::<StructuredDocument>(
            json!({"title":"x","schema_version":2,"blocks":[{
                "block_id":Uuid::nil(),"kind":"rich","text":"","items":[],"citation_ids":[],
                "rich":{"version":1,"node":node}
            }]}),
        ) {
            Err(_) => {}
            Ok(document) => assert_eq!(
                document.validate(&[]).unwrap_err().code,
                ErrorCode::InvalidRequest
            ),
        }
    }
    let mut nested = paragraph("deep");
    for _ in 0..30 {
        nested = json!({"type":"listItem","content":[paragraph("a"),{"type":"bulletList","content":[nested]}]});
    }
    let document = rich(json!({"type":"bulletList","content":[nested]}));
    assert_eq!(
        document.validate(&[]).unwrap_err().code,
        ErrorCode::InvalidRequest
    );
}
#[test]
fn markdown_preserves_marked_whitespace_adjacent_runs_and_list_paragraphs() {
    let document = valid(json!({"type":"bulletList","content":[
        {"type":"listItem","content":[
            {"type":"paragraph","content":[
                {"type":"text","text":" bold ","marks":[{"type":"bold"}]},
                {"type":"text","text":"one","marks":[{"type":"italic"}]},
                {"type":"text","text":"two","marks":[{"type":"italic"}]},
                {"type":"text","text":" x ","marks":[{"type":"code"}]}
            ]},
            {"type":"paragraph","content":[text("second paragraph")]}
        ]}
    ]}));
    let markdown = document.markdown();
    assert!(markdown.contains("<strong> bold </strong>"));
    assert!(markdown.contains("<em>one</em><em>two</em>"));
    assert!(markdown.contains("<code> x </code>"));
    assert!(markdown.contains("  \n  second paragraph"), "{markdown}");
    let sections = document.check_sections();
    assert!(
        sections[1].1.contains(" x \nsecond paragraph"),
        "{}",
        sections[1].1
    );
}
#[test]
fn markdown_ordered_list_continuations_follow_marker_width() {
    let document = valid(json!({"type":"orderedList","attrs":{"start":9},"content":[
        {"type":"listItem","content":[paragraph("first")]},
        {"type":"listItem","content":[
            paragraph("second"),
            paragraph("follow-up"),
            {"type":"bulletList","content":[{"type":"listItem","content":[paragraph("nested")]}]}
        ]}
    ]}));
    let markdown = document.markdown();
    assert!(
        markdown.contains("9. first\n10. second\n    \n    follow\\-up\n    - nested"),
        "{markdown}"
    );
    let nested = valid(json!({"type":"orderedList","attrs":{"start":10},"content":[
        {"type":"listItem","content":[
            paragraph("parent"),
            {"type":"orderedList","attrs":{"start":10},"content":[
                {"type":"listItem","content":[paragraph("child"),paragraph("later")]}
            ]}
        ]}
    ]}));
    assert!(
        nested
            .markdown()
            .contains("10. parent\n    10. child\n        \n        later"),
        "{}",
        nested.markdown()
    );
    let large = valid(
        json!({"type":"orderedList","attrs":{"start":4294967295u32},"content":[
            {"type":"listItem","content":[paragraph("large")]}
        ]}),
    );
    assert!(large.markdown().contains("<ol start=\"4294967295\">"));
}
#[test]
fn markdown_encodes_literal_html_and_entity_text_without_affecting_legacy() {
    let document = valid(paragraph(
        "Literal <script>alert(1)</script> & <em>x</em> &lt;",
    ));
    let markdown = document.markdown();
    assert!(!markdown.contains("<script>"), "{markdown}");
    assert!(!markdown.contains("<em>"), "{markdown}");
    assert!(markdown.contains("&lt;script"), "{markdown}");
    assert!(markdown.contains("&amp; &lt;em"), "{markdown}");
    assert!(markdown.contains("&amp;lt;"), "{markdown}");
    assert!(document.html(&[]).unwrap().contains("&lt;script&gt;"));
}
#[test]
fn standard_relative_anchor_and_mail_links_preserve_optional_title() {
    for href in [
        "#section",
        "/article",
        "./article",
        "../article",
        "article?q=1",
        "mailto:person@example.org",
    ] {
        let document = valid(json!({"type":"paragraph","content":[{
            "type":"text","text":"Label","marks":[{"type":"link","attrs":{"href":href,"title":"Open \"detail\""}}]
        }]}));
        assert_eq!(
            serde_json::from_value::<StructuredDocument>(serde_json::to_value(&document).unwrap())
                .unwrap(),
            document
        );
        assert!(
            document
                .html(&[])
                .unwrap()
                .contains("title=\"Open &quot;detail&quot;\"")
        );
        assert!(document.markdown().contains("Open \\\"detail\\\""));
    }
}
#[test]
fn media_references_are_structurally_valid_but_require_a_media_publication_adapter() {
    let object_id = Uuid::new_v4();
    let doc = rich(json!({"type":"media","attrs":{
        "object_id":object_id,"object_version":2,
        "sha256":"0123456789abcdef".repeat(4),"alt":"A < device","caption":"Caption & detail"
    }}));
    doc.validate_structure(&[]).unwrap();
    assert_eq!(doc.media_references()[0].object_id, object_id);
    assert!(
        doc.blocks[0]
            .rich
            .as_ref()
            .unwrap()
            .plain_text()
            .contains("Caption & detail")
    );
    assert!(doc.check_sections()[1].1.contains("A < device"));
    assert!(doc.check_sections()[1].1.contains("Caption & detail"));
    doc.validate(&[]).unwrap();
    let revision = revision(doc);
    assert_eq!(
        prepare_rich_variant(&revision, &placement(vec![RICH_MARKDOWN_FORMAT]))
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );
}
#[test]
fn bundle_rendering_resolves_only_typed_media_and_preserves_legacy_output() {
    let object_id = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
    let sha256 = "0123456789abcdef".repeat(4);
    let media = json!({"type":"media","attrs":{
        "object_id":object_id,"object_version":3,
        "sha256":sha256,"alt":"An <image> & [label]","caption":"Caption & <detail>"
    }});
    let mut document = valid(media.clone());
    document.title = "A <bundle> & title".into();
    let literal = format!("literal media/{object_id}-3 and ![x](media/{object_id}-3)");
    document.blocks.push(ContentBlock {
        block_id: Uuid::new_v4(),
        kind: ContentBlockKind::Rich,
        text: String::new(),
        items: vec![],
        citation_ids: vec![],
        rich: Some(geo_domain::RichContent {
            version: 1,
            node: serde_json::from_value(paragraph(&literal)).unwrap(),
        }),
    });
    document.blocks.push(ContentBlock {
        block_id: Uuid::new_v4(),
        kind: ContentBlockKind::Rich,
        text: String::new(),
        items: vec![],
        citation_ids: vec![],
        rich: Some(geo_domain::RichContent {
            version: 1,
            node: serde_json::from_value(
                json!({"type":"codeBlock","content":[text(&format!("media/{object_id}-3"))]}),
            )
            .unwrap(),
        }),
    });
    document.blocks.push(ContentBlock {
        block_id: Uuid::new_v4(),
        kind: ContentBlockKind::Rich,
        text: String::new(),
        items: vec![],
        citation_ids: vec![],
        rich: Some(geo_domain::RichContent {
            version: 1,
            node: serde_json::from_value(media).unwrap(),
        }),
    });
    document.validate(&[]).unwrap();
    let key = MediaObjectKey {
        object_id,
        object_version: 3,
        sha256,
    };
    let paths = HashMap::from([(key.clone(), format!("media/{object_id}-3.webp"))]);
    let original_markdown = document.markdown();
    let original_html = document.html(&[]).unwrap();
    let markdown = document.markdown_with_media_paths(&[], &paths).unwrap();
    let html = document.html_with_media_paths(&[], &paths).unwrap();
    assert_eq!(
        markdown
            .matches(&format!("](media/{object_id}-3.webp)"))
            .count(),
        2
    );
    assert_eq!(
        html.matches(&format!("src=\"media/{object_id}-3.webp\""))
            .count(),
        2
    );
    assert!(markdown.contains("An &lt;image\\> &amp; \\[label\\]"));
    assert!(markdown.contains("Caption &amp; &lt;detail\\>"));
    assert!(html.contains("alt=\"An &lt;image&gt; &amp; [label]\""));
    assert!(html.contains("<figcaption>Caption &amp; &lt;detail&gt;</figcaption>"));
    assert!(markdown.contains(&format!(
        "literal media/{}\\-3 and",
        object_id.to_string().replace('-', "\\-")
    )));
    assert!(markdown.contains(&format!("media/{object_id}-3\n```")));
    assert!(html.contains(&format!("literal media/{object_id}-3 and")));
    assert_eq!(document.markdown(), original_markdown);
    assert_eq!(document.html(&[]).unwrap(), original_html);
    assert!(!original_html.contains(".webp"));
    for missing in [
        HashMap::new(),
        HashMap::from([(
            MediaObjectKey {
                sha256: "f".repeat(64),
                ..key.clone()
            },
            format!("media/{object_id}-3.webp"),
        )]),
    ] {
        assert_eq!(
            document
                .markdown_with_media_paths(&[], &missing)
                .unwrap_err()
                .code,
            ErrorCode::InvalidRequest
        );
        assert_eq!(
            document
                .html_with_media_paths(&[], &missing)
                .unwrap_err()
                .code,
            ErrorCode::InvalidRequest
        );
    }
    let unsafe_paths = HashMap::from([(key, "../outside.png".into())]);
    assert_eq!(
        document
            .html_with_media_paths(&[], &unsafe_paths)
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );
    let mut conflicting = document.clone();
    if let geo_domain::RichNode::Media { attrs } = &mut conflicting
        .blocks
        .last_mut()
        .unwrap()
        .rich
        .as_mut()
        .unwrap()
        .node
    {
        attrs.sha256 = "f".repeat(64);
    }
    assert_eq!(
        conflicting
            .markdown_with_media_paths(&[], &paths)
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );
    assert_eq!(
        conflicting
            .html_with_media_paths(&[], &paths)
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );
    let nonmedia = valid(paragraph("Literal <text> & media/stays.png"));
    assert_eq!(
        nonmedia
            .markdown_with_media_paths(&[], &HashMap::new())
            .unwrap(),
        nonmedia.markdown()
    );
    assert_eq!(
        nonmedia
            .html_with_media_paths(&[], &HashMap::new())
            .unwrap(),
        nonmedia.html(&[]).unwrap()
    );
}
#[test]
fn rich_distribution_requires_explicit_format_and_separate_policy() {
    let doc = valid(paragraph("Exact rich text"));
    let rich_revision = revision(doc);
    assert_eq!(
        prepare_variant(&rich_revision, &placement(vec!["plain_text_article.v1"]))
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );
    assert_eq!(
        prepare_variant(&rich_revision, &placement(vec![RICH_MARKDOWN_FORMAT]))
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );
    let variant =
        prepare_rich_variant(&rich_revision, &placement(vec![RICH_MARKDOWN_FORMAT])).unwrap();
    assert_eq!(variant.policy_version, RICH_CHANNEL_VARIANT_POLICY);
    assert_eq!(variant.markdown, rich_revision.markdown);
    let legacy = StructuredDocument {
        title: "Legacy".into(),
        blocks: vec![ContentBlock {
            block_id: Uuid::new_v4(),
            kind: ContentBlockKind::Paragraph,
            text: "old".into(),
            citation_ids: vec![],
            items: vec![],
            rich: None,
        }],
        schema_version: None,
    };
    assert_eq!(
        prepare_variant(&revision(legacy), &placement(vec!["faq"]))
            .unwrap()
            .policy_version,
        CHANNEL_VARIANT_POLICY
    );
}
