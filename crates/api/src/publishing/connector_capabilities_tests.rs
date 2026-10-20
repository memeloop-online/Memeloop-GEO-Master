use super::*;
use chrono::Utc;
use geo_domain::{
    ChannelOutcome, ChannelOutcomeStatus, ConnectorCapabilityRepository, ConnectorVerification,
    MemoryChannelRepository, MemoryConnectorCapabilityRepository, MemoryContentRepository,
    MemoryDistributionRepository, MemoryKnowledgeRepository, MemoryProjectRepository,
};
use std::{collections::BTreeSet, sync::Arc};
use uuid::Uuid;

#[tokio::test]
async fn explicit_request_formats_discover_deployed_placements_without_claiming_verification() {
    let state = AppState::development();
    let operator = Uuid::new_v4().into();
    let key = ConnectorKey {
        platform_id: "zhihu".into(),
        placement_slot: "short_post".into(),
    };
    let connector = crate::browser_bridge::RunnerConnector {
        platform: key.platform_id.clone(),
        placement_slot: key.placement_slot.clone(),
        connector_version: "short.v1".into(),
        operations: vec!["publish".into(), "lookup".into()],
        login_entry_available: None,
        login_supported: None,
        model_discovery_supported: false,
        verified: false,
    };
    let connectors = vec![connector.clone()];
    assert!(catalog(&[], &connectors).contains(&key));
    let entry = view(&state, None, key.clone(), &connectors, operator, false)
        .await
        .unwrap();
    assert_eq!(entry.availability, ConnectorAvailability::Unavailable);
    assert_eq!(
        entry.requestable_formats,
        [geo_domain::TEXT_DISTRIBUTION_FORMAT]
    );
    assert!(entry.content_types.is_empty());
    assert!(entry.verified_content_types.is_none());

    for (enabled, content_types, allowed) in [
        (false, vec![PLAIN_TEXT_ARTICLE_FORMAT.into()], false),
        (true, vec![RICH_MARKDOWN_FORMAT.into()], false),
        (true, vec!["image".into()], false),
        (true, vec!["faq".into()], true),
        (true, vec![PLAIN_TEXT_ARTICLE_FORMAT.into()], true),
    ] {
        let settings = ConnectorSettings {
            key: key.clone(),
            revision: 1,
            enabled,
            content_types,
        };
        let entry = view(
            &state,
            Some(settings),
            key.clone(),
            &connectors,
            operator,
            false,
        )
        .await
        .unwrap();
        assert_eq!(!entry.requestable_formats.is_empty(), allowed);
        assert_ne!(entry.availability, ConnectorAvailability::Available);
    }
    let mut article_only = connector.clone();
    article_only.placement_slot = "primary".into();
    assert!(!requested_plaintext_available(
        &[article_only],
        &key,
        None,
        None
    ));
    let mut missing_lookup = connector.clone();
    missing_lookup.operations = vec!["publish".into()];
    assert!(!requested_plaintext_available(
        &[missing_lookup],
        &key,
        None,
        None
    ));
    assert!(!requested_plaintext_available(
        &[connector.clone(), connector],
        &key,
        None,
        None
    ));
}

#[test]
fn configured_publication_format_is_explicit_and_excludes_media() {
    let settings = ConnectorSettings {
        key: ConnectorKey {
            platform_id: "example".into(),
            placement_slot: "primary".into(),
        },
        revision: 1,
        enabled: true,
        content_types: vec![PLAIN_TEXT_ARTICLE_FORMAT.into()],
    };
    for semantic in ["faq", "company_profile", "article"] {
        assert_eq!(
            configured_publication_format(&settings, semantic),
            Some(PLAIN_TEXT_ARTICLE_FORMAT)
        );
    }
    assert_eq!(
        configured_source_format(&settings),
        Some(PLAIN_TEXT_ARTICLE_FORMAT)
    );
    for semantic in ["image", "video", "rich_text", "plain_text_article.v1"] {
        assert_eq!(configured_publication_format(&settings, semantic), None);
    }
    assert_eq!(configured_rich_publication_format(&settings), None);

    let legacy = ConnectorSettings {
        content_types: vec!["faq".into()],
        ..settings
    };
    assert_eq!(configured_publication_format(&legacy, "faq"), Some("faq"));
    assert_eq!(
        configured_publication_format(&legacy, "company_profile"),
        None
    );
    assert_eq!(configured_source_format(&legacy), None);
    let rich = ConnectorSettings {
        content_types: vec![RICH_MARKDOWN_FORMAT.into()],
        ..legacy
    };
    assert_eq!(
        configured_rich_publication_format(&rich),
        Some(RICH_MARKDOWN_FORMAT)
    );
    assert_eq!(configured_publication_format(&rich, "article"), None);
    assert_eq!(configured_source_format(&rich), None);
}

#[tokio::test]
async fn saved_wire_proof_resolves_known_semantics_but_not_media_or_old_source_proofs() {
    let repo = MemoryConnectorCapabilityRepository::default();
    let operator = Uuid::new_v4().into();
    let key = ConnectorKey {
        platform_id: "creator".into(),
        placement_slot: "primary".into(),
    };
    let now = Utc::now();
    let url = "https://example.com/public".to_owned();
    let hash = "a".repeat(64);
    let receipt = ChannelOutcome {
        status: ChannelOutcomeStatus::Published,
        detail: None,
        occurred_at: now,
        raw_answer: None,
        citations: vec![],
        public_url: Some(url.clone()),
        screenshot_ref: None,
        connector_version: Some("live.v1".into()),
        runner_evidence: vec![],
        fixture: false,
    };
    let proof = ConnectorVerification {
        verification_id: Uuid::new_v4(),
        key: key.clone(),
        connector_version: "live.v1".into(),
        content_type: PLAIN_TEXT_ARTICLE_FORMAT.into(),
        publication_receipt: receipt.clone(),
        public_readback: ChannelOutcome {
            status: ChannelOutcomeStatus::Verified,
            runner_evidence: vec![serde_json::json!({
                "kind":"public_readback", "url":url,
                "content_matched":true, "owned_by_account":true,
                "expected_sha256":hash, "readback_sha256":hash
            })],
            ..receipt
        },
        verified_at: now,
    };
    repo.insert_verification(operator, proof).await.unwrap();
    let settings = repo
        .configure(
            operator,
            key.clone(),
            0,
            true,
            vec![PLAIN_TEXT_ARTICLE_FORMAT.into()],
            "live.v1",
        )
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let runner = tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new().route(
                "/v1/capabilities",
                axum::routing::get(|| async {
                    axum::Json(serde_json::json!({"connectors":[{
                        "platform":"creator", "placement_slot":"primary",
                        "connector_version":"live.v1", "operations":["publish"],
                        "verified":false
                    }]}))
                }),
            ),
        )
        .await
        .unwrap();
    });
    let service = crate::distribution::DistributionService::new(
        Arc::new(MemoryDistributionRepository::default()),
        Arc::new(MemoryContentRepository::default()),
        Arc::new(MemoryKnowledgeRepository::default()),
        Arc::new(MemoryProjectRepository::default()),
        Arc::new(MemoryChannelRepository::default()),
    )
    .with_connector_registry(
        Arc::new(repo.clone()),
        Some(crate::BrowserBridge::new(format!("http://{address}"), "test-token".into()).unwrap()),
    );
    let supported = service
        .current_capabilities(
            operator,
            &BTreeSet::from([
                "faq".into(),
                "company_profile".into(),
                "image".into(),
                RICH_MARKDOWN_FORMAT.into(),
            ]),
        )
        .await
        .unwrap();
    let frozen = supported
        .iter()
        .find(|item| item.platform_id == "creator")
        .unwrap();
    assert_eq!(
        frozen.supported_formats,
        vec!["company_profile", "faq"],
        "manifest snapshot retains semantic content types, never the wire key"
    );
    assert_eq!(configured_rich_publication_format(&settings), None);
    assert_eq!(frozen.unavailable_reason, None);
    runner.abort();
    for semantic in ["faq", "company_profile"] {
        let selected = configured_publication_format(&settings, semantic).unwrap();
        assert_eq!(
            repo.resolve(operator, &key, "live.v1", selected)
                .await
                .unwrap()
                .availability,
            ConnectorAvailability::Available
        );
    }
    assert_eq!(configured_publication_format(&settings, "image"), None);
    assert_eq!(
        configured_source_format(&settings),
        Some(PLAIN_TEXT_ARTICLE_FORMAT)
    );
    assert_eq!(
        repo.resolve(operator, &key, "live.v2", PLAIN_TEXT_ARTICLE_FORMAT)
            .await
            .unwrap()
            .availability,
        ConnectorAvailability::VersionMismatch
    );
    repo.configure(
        operator,
        key.clone(),
        settings.revision,
        false,
        vec![PLAIN_TEXT_ARTICLE_FORMAT.into()],
        "live.v1",
    )
    .await
    .unwrap();
    assert_eq!(
        repo.resolve(operator, &key, "live.v1", PLAIN_TEXT_ARTICLE_FORMAT)
            .await
            .unwrap()
            .availability,
        ConnectorAvailability::Disabled
    );
}
