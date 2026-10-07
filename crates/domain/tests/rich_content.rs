use chrono::Utc;
use geo_domain::{
    CHANNEL_VARIANT_POLICY, ContentBlock, ContentBlockKind, ContentRevision, ErrorCode,
    MediaObjectKey, PlatformPlacement, RICH_CHANNEL_VARIANT_POLICY, RICH_MARKDOWN_FORMAT,
    StructuredDocument, prepare_rich_variant, prepare_variant,
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
