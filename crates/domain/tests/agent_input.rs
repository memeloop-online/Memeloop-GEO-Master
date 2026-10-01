use geo_domain::{
    AgentRepository, AppendMessage, AttachmentReference, CreateConversation, MemoryAgentRepository,
    RuntimeCapability, TenantScope,
};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test]
async fn accepted_attachment_only_input_reconstructs_from_durable_message_identity() {
    let repository = MemoryAgentRepository::new();
    let scope = TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    );
    let conversation = repository
        .create_conversation(&scope, None, CreateConversation::default())
        .await
        .unwrap();
    let attachment = AttachmentReference {
        attachment_id: Uuid::new_v4().into(),
        object_id: Uuid::new_v4().to_string(),
        filename: "notes.md".into(),
        media_type: Some("text/markdown".into()),
        size_bytes: Some(42),
        sha256: Some("a".repeat(64)),
        object_version: Some("1".into()),
    };
    let duplicate = AppendMessage {
        content: String::new(),
        attachments: vec![attachment.clone(), attachment.clone()],
        metadata: Value::Null,
    };
    assert!(geo_domain::validate_append_message(&duplicate).is_err());
    let acceptance = repository
        .append_message(
            &scope,
            conversation.id,
            AppendMessage {
                content: String::new(),
                attachments: vec![attachment.clone()],
                metadata: Value::Null,
            },
            "input-key".into(),
            "input-hash".into(),
            RuntimeCapability::available("fixture", None),
        )
        .await
        .unwrap();
    let detail = repository
        .get_conversation(&scope, conversation.id)
        .await
        .unwrap()
        .unwrap();
    let input = detail.turn_input(acceptance.run.id).unwrap();
    assert_eq!(input.message_id, acceptance.message.id);
    assert_eq!(input.turn_id, acceptance.turn.id);
    assert_eq!(input.attachments, vec![attachment]);
    assert!(input.prompt.is_empty());
    let reloaded = repository
        .get_conversation(&scope, conversation.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(input, reloaded.turn_input(acceptance.run.id).unwrap());
    assert!(detail.turn_input(Uuid::new_v4().into()).is_err());
    let mut broken = detail;
    broken.turns[0].root_message_id = Uuid::new_v4().into();
    assert!(broken.turn_input(acceptance.run.id).is_err());
}
