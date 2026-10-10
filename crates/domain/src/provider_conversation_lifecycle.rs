//! Closed provider registry for transport lifecycle evidence, not answer
//! interpretation or account-wide cleanup authority.

use std::collections::HashMap;

use crate::{ObservationCaptureCompletion, ObservationCompletionProtocol};

type InventoryVerifier = for<'a> fn(
    &'a serde_json::Value,
    &str,
    &ObservationCaptureCompletion,
) -> Option<Vec<(&'a str, &'static str)>>;

struct LifecycleAdapter {
    protocol: ObservationCompletionProtocol,
    inventory: InventoryVerifier,
    cleanup_supported: bool,
}

const KIMI: LifecycleAdapter = LifecycleAdapter {
    protocol: ObservationCompletionProtocol::ConnectJson,
    inventory: kimi_completion_inventory,
    cleanup_supported: true,
};

fn adapter(provider: &str) -> Option<&'static LifecycleAdapter> {
    match provider {
        "kimi" => Some(&KIMI),
        _ => None,
    }
}

/// Protocol support only. Exact ownership, durable evidence, original identity,
/// current account reservation and deletion authorization remain mandatory.
pub fn provider_conversation_cleanup_supported(provider: &str) -> bool {
    adapter(provider).is_some_and(|adapter| adapter.cleanup_supported)
}

pub(crate) fn completion_inventory<'a>(
    provider: &str,
    document: &'a serde_json::Value,
    chat_id: &str,
    completion: &ObservationCaptureCompletion,
) -> Option<Vec<(&'a str, &'static str)>> {
    let adapter = adapter(provider)?;
    if completion.protocol != adapter.protocol {
        return None;
    }
    (adapter.inventory)(document, chat_id, completion)
}

fn valid_message_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn kimi_completion_inventory<'a>(
    document: &'a serde_json::Value,
    chat_id: &str,
    completion: &ObservationCaptureCompletion,
) -> Option<Vec<(&'a str, &'static str)>> {
    use serde_json::Value;
    if !completion.terminal
        || completion.assistant_message_ids.is_empty()
        || completion.assistant_message_ids.len() > 256
        || completion
            .assistant_message_ids
            .iter()
            .any(|id| !valid_message_id(id))
    {
        return None;
    }
    let envelopes = document.get("messages").and_then(Value::as_array)?;
    #[derive(Default)]
    struct MessageState {
        role: Option<&'static str>,
        completed: bool,
    }
    let mut states: HashMap<&str, MessageState> = HashMap::new();
    let mut order = Vec::new();
    let mut saw_chat = false;
    for envelope in envelopes {
        let fields = envelope.as_object()?;
        if fields.get("error").is_some_and(|value| !value.is_null())
            || ["chat", "message", "block", "ref", "done"]
                .iter()
                .filter(|key| fields.get(**key).is_some_and(|value| !value.is_null()))
                .count()
                > 1
        {
            return None;
        }
        if let Some(chat) = fields.get("chat").filter(|value| !value.is_null()) {
            if chat.get("id").and_then(Value::as_str) != Some(chat_id) {
                return None;
            }
            saw_chat = true;
        }
        let Some(message) = fields.get("message").filter(|value| !value.is_null()) else {
            continue;
        };
        // The exact preceding chat envelope establishes the stream's scope.
        // Message-local chat IDs are optional in this provider's transport.
        if !saw_chat {
            return None;
        }
        let message = message.as_object()?;
        let id = message
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| valid_message_id(id))?;
        if !states.contains_key(id) {
            order.push(id);
        }
        let state = states.entry(id).or_default();
        if let Some(role) = message.get("role") {
            let role = match (role.as_str(), role.as_u64()) {
                (Some("assistant"), _) | (_, Some(3)) => "assistant",
                (Some("user"), _) | (_, Some(2)) => "user",
                (Some("system"), _) => "system",
                _ => return None,
            };
            if state.role.is_some_and(|prior| prior != role) {
                return None;
            }
            state.role = Some(role);
        }
        if let Some(chat) = message.get("chat_id")
            && chat.as_str() != Some(chat_id)
        {
            return None;
        }
        if let Some(status) = message.get("status") {
            state.completed = matches!(
                status.as_str(),
                Some("COMPLETED" | "MESSAGE_STATUS_COMPLETED")
            ) || status.as_u64() == Some(2);
        }
    }
    let mut assistants = Vec::new();
    let mut inventory = Vec::new();
    for id in order {
        let state = &states[id];
        match state.role {
            Some("assistant") if state.completed => {
                assistants.push(id);
                inventory.push((id, "assistant"));
            }
            Some(role @ ("user" | "system")) => inventory.push((id, role)),
            _ => return None,
        }
    }
    let matched = saw_chat
        && assistants
            .iter()
            .copied()
            .eq(completion.assistant_message_ids.iter().map(String::as_str));
    inventory.sort_unstable_by_key(|(id, _)| *id);
    matched.then_some(inventory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_registered_provider_can_use_kimi_evidence_or_cleanup_protocol() {
        let document = serde_json::json!({"messages": [
            {"chat": {"id": "synthetic-chat"}},
            {"message": {"id": "synthetic-answer", "role": "assistant", "status": "COMPLETED"}}
        ]});
        let completion = ObservationCaptureCompletion {
            protocol: ObservationCompletionProtocol::ConnectJson,
            terminal: true,
            assistant_message_ids: vec!["synthetic-answer".into()],
        };
        for provider in ["", "Kimi", "deepseek", "doubao", "glm", "unknown"] {
            assert!(!provider_conversation_cleanup_supported(provider));
            assert!(
                completion_inventory(provider, &document, "synthetic-chat", &completion).is_none()
            );
        }
        assert!(provider_conversation_cleanup_supported("kimi"));
        assert_eq!(
            completion_inventory("kimi", &document, "synthetic-chat", &completion),
            Some(vec![("synthetic-answer", "assistant")]),
        );
    }
}
