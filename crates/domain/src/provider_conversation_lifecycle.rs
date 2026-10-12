//! Closed provider registry for transport lifecycle evidence, not answer
//! interpretation or account-wide cleanup authority.

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
};

use chrono::DateTime;
use serde_json::Value;

use crate::{ObservationCaptureCompletion, ObservationCompletionProtocol};

type MessageInventory<'a> = Vec<(Cow<'a, str>, &'static str)>;
type InventoryVerifier = for<'a> fn(
    &'a serde_json::Value,
    &str,
    &ObservationCaptureCompletion,
) -> Option<MessageInventory<'a>>;

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

const DEEPSEEK: LifecycleAdapter = LifecycleAdapter {
    protocol: ObservationCompletionProtocol::DeepseekHistoryJson,
    inventory: deepseek_completion_inventory,
    // Completion evidence alone does not implement remote deletion.
    cleanup_supported: false,
};

fn adapter(provider: &str) -> Option<&'static LifecycleAdapter> {
    match provider {
        "kimi" => Some(&KIMI),
        "deepseek" => Some(&DEEPSEEK),
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
) -> Option<MessageInventory<'a>> {
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
) -> Option<MessageInventory<'a>> {
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
                inventory.push((Cow::Borrowed(id), "assistant"));
            }
            Some(role @ ("user" | "system")) => inventory.push((Cow::Borrowed(id), role)),
            _ => return None,
        }
    }
    let matched = saw_chat
        && assistants
            .iter()
            .copied()
            .eq(completion.assistant_message_ids.iter().map(String::as_str));
    inventory.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
    matched.then_some(inventory)
}

fn closed_fields(value: &Value, required: &[&str], optional: &[&str]) -> bool {
    value.as_object().is_some_and(|fields| {
        required.iter().all(|key| fields.contains_key(*key))
            && fields
                .keys()
                .all(|key| required.contains(&key.as_str()) || optional.contains(&key.as_str()))
    })
}

fn deepseek_message_id(value: &Value) -> Option<u64> {
    value.as_u64().filter(|id| *id <= 9_007_199_254_740_991)
}

struct DeepseekLifecycle<'a> {
    chat_id: &'a str,
    model: &'a str,
    request_id: u64,
    response_id: u64,
}

/// This is a closed transport projection made by the authenticated runner.
/// Its graph shape is checked here; actual SSE correlation is checked separately
/// before it can grant completion. It says nothing about answer/search semantics.
fn deepseek_lifecycle(value: &Value) -> Option<DeepseekLifecycle<'_>> {
    if !closed_fields(
        value,
        &[
            "schema_version",
            "provider",
            "creation",
            "submission",
            "ready",
            "sse_terminal",
            "history",
        ],
        &[],
    ) || value["schema_version"] != "geo.deepseek.lifecycle.v1"
        || value["provider"] != "deepseek"
        || value["sse_terminal"] != true
    {
        return None;
    }
    let creation = &value["creation"];
    let submission = &value["submission"];
    let ready = &value["ready"];
    let history = &value["history"];
    if !closed_fields(creation, &["source", "chat_session_id"], &[])
        || creation["source"] != "browser_observed_create"
        || !closed_fields(
            submission,
            &[
                "chat_session_id",
                "parent_message_id",
                "model_type",
                "prompt_sha256",
                "search_enabled",
                "thinking_enabled",
            ],
            &[],
        )
        || submission["parent_message_id"] != Value::Null
        || submission["search_enabled"] != true
        || submission["thinking_enabled"] != false
        || !closed_fields(ready, &["request_message_id", "response_message_id"], &[])
        || !closed_fields(
            history,
            &[
                "source",
                "observed_at",
                "cache_request",
                "cache_control",
                "chat_session_id",
                "messages",
            ],
            &["current_message_id"],
        )
        || history["source"] != "browser_observed_history"
        || history["cache_control"] != "REPLACE"
        || !closed_fields(
            &history["cache_request"],
            &["cache_version", "cache_reset_at"],
            &[],
        )
        || history["cache_request"]["cache_version"] != Value::Null
        || history["cache_request"]["cache_reset_at"] != Value::Null
    {
        return None;
    }
    let chat_id = creation["chat_session_id"]
        .as_str()
        .filter(|id| valid_message_id(id))?;
    let model = submission["model_type"].as_str().filter(|model| {
        !model.is_empty()
            && model.len() <= 128
            && model
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    })?;
    let digest = submission["prompt_sha256"].as_str()?;
    let observed_at = history["observed_at"].as_str()?;
    if submission["chat_session_id"].as_str() != Some(chat_id)
        || history["chat_session_id"].as_str() != Some(chat_id)
        || digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || observed_at.len() > 64
        || DateTime::parse_from_rfc3339(observed_at).is_err()
    {
        return None;
    }
    let request_id = deepseek_message_id(&ready["request_message_id"])?;
    let response_id = deepseek_message_id(&ready["response_message_id"])?;
    if request_id == response_id
        || history
            .get("current_message_id")
            .is_some_and(|id| !id.is_null() && deepseek_message_id(id) != Some(response_id))
    {
        return None;
    }
    let messages = history["messages"].as_array()?;
    if messages.len() != 2 {
        return None;
    }
    let mut saw_user = false;
    let mut saw_assistant = false;
    for message in messages {
        if !closed_fields(
            message,
            &[
                "message_id",
                "parent_id",
                "role",
                "status",
                "has_pending_fragment",
            ],
            &["quasi_status"],
        ) || message["status"] != "FINISHED"
            || message["has_pending_fragment"] != false
            || message
                .get("quasi_status")
                .is_some_and(|status| !status.is_null() && status != "FINISHED")
        {
            return None;
        }
        match message["role"].as_str() {
            Some("USER")
                if !saw_user
                    && deepseek_message_id(&message["message_id"]) == Some(request_id)
                    && message["parent_id"].is_null() =>
            {
                saw_user = true
            }
            Some("ASSISTANT")
                if !saw_assistant
                    && deepseek_message_id(&message["message_id"]) == Some(response_id)
                    && deepseek_message_id(&message["parent_id"]) == Some(request_id) =>
            {
                saw_assistant = true
            }
            _ => return None,
        }
    }
    (saw_user && saw_assistant).then_some(DeepseekLifecycle {
        chat_id,
        model,
        request_id,
        response_id,
    })
}

pub(crate) fn deepseek_lifecycle_chat_id(value: &Value) -> Option<&str> {
    deepseek_lifecycle(value).map(|lifecycle| lifecycle.chat_id)
}

fn deepseek_completion_inventory<'a>(
    document: &'a Value,
    chat_id: &str,
    completion: &ObservationCaptureCompletion,
) -> Option<MessageInventory<'a>> {
    let lifecycle = deepseek_lifecycle(document.get("lifecycle")?)?;
    if lifecycle.chat_id != chat_id
        || !completion.terminal
        || completion.assistant_message_ids != [lifecycle.response_id.to_string()]
    {
        return None;
    }
    let mut ready = false;
    let mut finished = false;
    let messages = document.get("messages")?.as_array()?;
    if messages.len() > 32_768 {
        return None;
    }
    let mut omitted_paths = HashSet::new();
    if document.get("source_redaction").is_some() {
        // This shared check validates the lifecycle shape, not completion,
        // so it cannot recursively enter this inventory verifier.
        if !crate::observation_capture::valid_observation_source_document(document) {
            return None;
        }
        for path in document["source_redaction"]["omitted_paths"].as_array()? {
            if !omitted_paths.insert(path.as_str()?) {
                return None;
            }
        }
    }
    for (index, message) in messages.iter().enumerate() {
        let event = message.get("event")?.as_str()?;
        if event == "finish" {
            // Do not restore or interpret opaque payloads removed by projection.
            let finish_payload = match message.get("raw_data") {
                Some(value) => value.is_string(),
                None => omitted_paths.contains(format!("/messages/{index}/raw_data").as_str()),
            };
            if !ready || finished || !finish_payload {
                return None;
            }
            finished = true;
            continue;
        }
        let data = message.get("data")?.as_object()?;
        if event == "ready" {
            if ready
                || finished
                || data.get("request_message_id").and_then(deepseek_message_id)
                    != Some(lifecycle.request_id)
                || data
                    .get("response_message_id")
                    .and_then(deepseek_message_id)
                    != Some(lifecycle.response_id)
                || data.get("model_type").is_some_and(|model| {
                    !model.is_null() && model.as_str() != Some(lifecycle.model)
                })
            {
                return None;
            }
            ready = true;
        } else if matches!(event, "" | "message" | "delta") && (!ready || finished) {
            return None;
        }
    }
    if !ready || !finished {
        return None;
    }
    let mut inventory: MessageInventory<'a> = vec![
        (Cow::Owned(lifecycle.request_id.to_string()), "user"),
        (Cow::Owned(lifecycle.response_id.to_string()), "assistant"),
    ];
    inventory.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
    Some(inventory)
}

#[cfg(test)]
pub(crate) fn synthetic_deepseek_source() -> Value {
    serde_json::json!({
        "messages": [
            {"event": "ready", "data": {"request_message_id": 1, "response_message_id": 2, "model_type": "synthetic-model"}},
            {"event": "delta", "data": {"v": "synthetic answer"}},
            {"event": "finish", "raw_data": ""}
        ],
        "lifecycle": {
            "schema_version": "geo.deepseek.lifecycle.v1",
            "provider": "deepseek",
            "creation": {"source": "browser_observed_create", "chat_session_id": "synthetic-chat"},
            "submission": {
                "chat_session_id": "synthetic-chat", "parent_message_id": null,
                "model_type": "synthetic-model", "prompt_sha256": "0".repeat(64),
                "search_enabled": true, "thinking_enabled": false
            },
            "ready": {"request_message_id": 1, "response_message_id": 2},
            "sse_terminal": true,
            "history": {
                "source": "browser_observed_history", "observed_at": "2026-01-01T00:00:00Z",
                "cache_request": {"cache_version": null, "cache_reset_at": null},
                "cache_control": "REPLACE", "chat_session_id": "synthetic-chat",
                "messages": [
                    {"message_id": 1, "parent_id": null, "role": "USER", "status": "FINISHED", "has_pending_fragment": false},
                    {"message_id": 2, "parent_id": 1, "role": "ASSISTANT", "status": "FINISHED", "has_pending_fragment": false}
                ],
                "current_message_id": 2
            }
        }
    })
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
            Some(vec![(Cow::Borrowed("synthetic-answer"), "assistant")]),
        );
    }

    fn deepseek_completion() -> ObservationCaptureCompletion {
        ObservationCaptureCompletion {
            protocol: ObservationCompletionProtocol::DeepseekHistoryJson,
            terminal: true,
            assistant_message_ids: vec!["2".into()],
        }
    }

    fn complete(document: &Value) -> bool {
        completion_inventory(
            "deepseek",
            document,
            "synthetic-chat",
            &deepseek_completion(),
        )
        .is_some()
    }

    fn redacted_deepseek_source() -> Value {
        let mut document = synthetic_deepseek_source();
        document["messages"][2]
            .as_object_mut()
            .unwrap()
            .remove("raw_data");
        document["source_redaction"] = serde_json::json!({
            "schema_version": "geo.observation.source-redaction.v1",
            "policy": "reasoning-and-private-fields",
            "omitted_paths": ["/messages/2/raw_data"]
        });
        document
    }

    #[test]
    fn deepseek_redacted_finish_preserves_completion_without_opaque_payload() {
        let document = redacted_deepseek_source();
        let saved: Value = serde_json::from_str(&document.to_string()).unwrap();
        assert!(complete(&saved));
        assert_eq!(
            completion_inventory("deepseek", &saved, "synthetic-chat", &deepseek_completion()),
            completion_inventory(
                "deepseek",
                &synthetic_deepseek_source(),
                "synthetic-chat",
                &deepseek_completion()
            )
        );
        assert!(saved["messages"][2].get("raw_data").is_none());
        let mut shifted = saved;
        shifted["messages"]
            .as_array_mut()
            .unwrap()
            .insert(0, serde_json::json!({"event": "unknown", "data": {}}));
        shifted["source_redaction"]["omitted_paths"] = serde_json::json!(["/messages/3/raw_data"]);
        assert!(complete(&shifted));
    }

    #[test]
    fn deepseek_redacted_finish_requires_closed_unique_exact_absent_paths() {
        let document = redacted_deepseek_source();
        for paths in [
            serde_json::json!([]),
            serde_json::json!(["/messages/1/raw_data"]),
            serde_json::json!(["/messages/02/raw_data"]),
            serde_json::json!(["/messages/2/data"]),
            serde_json::json!(["/messages/2/raw_data", "/messages/2/raw_data"]),
            serde_json::json!(["/messages/2/raw_data", null]),
            serde_json::json!(["/messages/2/raw_data", "/messages/1/~2"]),
            serde_json::json!(["/messages/2/raw_data", "/messages/3/raw_data"]),
            serde_json::json!(["/messages/2/raw_data", "/messages/0/data"]),
        ] {
            let mut invalid = document.clone();
            invalid["source_redaction"]["omitted_paths"] = paths;
            assert!(!complete(&invalid), "{}", invalid["source_redaction"]);
        }
        for (path, value) in [
            ("/source_redaction", Value::Null),
            (
                "/source_redaction/schema_version",
                serde_json::json!("unknown"),
            ),
            ("/source_redaction/policy", serde_json::json!("unknown")),
            ("/messages/2/event", serde_json::json!("close")),
            ("/messages/0/data/response_message_id", serde_json::json!(3)),
            (
                "/lifecycle/history/messages/1/message_id",
                serde_json::json!(3),
            ),
        ] {
            let mut invalid = document.clone();
            *invalid.pointer_mut(path).unwrap() = value;
            assert!(!complete(&invalid), "{path}");
        }
        for raw_data in [Value::Null, serde_json::json!("")] {
            let mut invalid = document.clone();
            invalid["messages"][2]["raw_data"] = raw_data;
            assert!(!complete(&invalid));
        }
        let mut invalid = document.clone();
        invalid["source_redaction"]["extra"] = serde_json::json!(true);
        assert!(!complete(&invalid));
        for (path, key) in [
            ("", "source_redaction"),
            ("/source_redaction", "omitted_paths"),
            ("/lifecycle", "history"),
        ] {
            let mut invalid = document.clone();
            invalid
                .pointer_mut(path)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(key);
            assert!(!complete(&invalid), "missing {path}/{key}");
        }
        for index in [0, 2] {
            let mut invalid = document.clone();
            invalid["messages"].as_array_mut().unwrap().remove(index);
            assert!(!complete(&invalid), "missing event {index}");
        }
    }

    #[test]
    fn deepseek_requires_independent_sse_and_history_correlation() {
        let document = synthetic_deepseek_source();
        assert!(complete(&document));
        assert_eq!(
            completion_inventory(
                "deepseek",
                &document,
                "synthetic-chat",
                &deepseek_completion()
            ),
            Some(vec![
                (Cow::Owned("1".into()), "user"),
                (Cow::Owned("2".into()), "assistant")
            ])
        );
        assert!(!provider_conversation_cleanup_supported("deepseek"));
        for provider in ["kimi", "DeepSeek", "unknown"] {
            assert!(
                completion_inventory(
                    provider,
                    &document,
                    "synthetic-chat",
                    &deepseek_completion()
                )
                .is_none()
            );
        }
        assert!(
            completion_inventory("deepseek", &document, "other-chat", &deepseek_completion())
                .is_none()
        );
        for ids in [
            vec![],
            vec!["02".into()],
            vec!["2.0".into()],
            vec!["1".into()],
            vec!["2".into(), "2".into()],
        ] {
            let mut completion = deepseek_completion();
            completion.assistant_message_ids = ids;
            assert!(
                completion_inventory("deepseek", &document, "synthetic-chat", &completion)
                    .is_none()
            );
        }
        let mut completion = deepseek_completion();
        completion.terminal = false;
        assert!(
            completion_inventory("deepseek", &document, "synthetic-chat", &completion).is_none()
        );
        completion = deepseek_completion();
        completion.protocol = ObservationCompletionProtocol::ConnectJson;
        assert!(
            completion_inventory("deepseek", &document, "synthetic-chat", &completion).is_none()
        );
        for (path, value) in [
            ("/messages/0/data/request_message_id", serde_json::json!(3)),
            (
                "/messages/0/data/response_message_id",
                serde_json::json!("2"),
            ),
            (
                "/messages/0/data/model_type",
                serde_json::json!("other-model"),
            ),
            ("/messages/0/event", serde_json::json!("delta")),
            ("/messages/2/event", serde_json::json!("close")),
            ("/messages/2/raw_data", Value::Null),
            ("/messages", serde_json::json!([])),
        ] {
            let mut invalid = document.clone();
            *invalid.pointer_mut(path).unwrap() = value;
            assert!(!complete(&invalid), "{path}");
            assert!(deepseek_lifecycle_chat_id(&invalid["lifecycle"]).is_some());
        }
        for index in [0, 2] {
            let mut invalid = document.clone();
            invalid["messages"]
                .as_array_mut()
                .unwrap()
                .push(document["messages"][index].clone());
            assert!(!complete(&invalid));
        }
        let mut extended = document.clone();
        extended["messages"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"event": "close", "data": {}}));
        assert!(complete(&extended));
        for value in [
            serde_json::json!({"data": {}}),
            serde_json::json!({"event": "close", "data": null}),
            serde_json::json!({"event": "close"}),
        ] {
            let mut invalid = document.clone();
            invalid["messages"].as_array_mut().unwrap().push(value);
            assert!(!complete(&invalid));
        }
        extended["messages"]
            .as_array_mut()
            .unwrap()
            .resize(32_769, serde_json::json!({"event": "close", "data": {}}));
        assert!(!complete(&extended));
        for (left, right) in [(0, 1), (0, 2), (1, 2)] {
            let mut invalid = document.clone();
            invalid["messages"]
                .as_array_mut()
                .unwrap()
                .swap(left, right);
            assert!(!complete(&invalid));
        }
    }

    #[test]
    fn deepseek_closed_metadata_rejects_invalid_graph_and_scalars() {
        let document = synthetic_deepseek_source();
        for (path, value) in [
            ("/schema_version", serde_json::json!("unknown")),
            ("/provider", serde_json::json!("kimi")),
            ("/creation/source", serde_json::json!("model_inferred")),
            ("/creation/chat_session_id", serde_json::json!("invalid/id")),
            (
                "/creation/chat_session_id",
                serde_json::json!("x".repeat(129)),
            ),
            (
                "/submission/chat_session_id",
                serde_json::json!("other-chat"),
            ),
            ("/submission/parent_message_id", serde_json::json!(0)),
            ("/submission/model_type", serde_json::json!("")),
            ("/submission/model_type", serde_json::json!("x".repeat(129))),
            ("/submission/model_type", serde_json::json!("model\n")),
            (
                "/submission/prompt_sha256",
                serde_json::json!("A".repeat(64)),
            ),
            (
                "/submission/prompt_sha256",
                serde_json::json!("0".repeat(63)),
            ),
            ("/submission/search_enabled", serde_json::json!(false)),
            ("/submission/thinking_enabled", serde_json::json!(true)),
            ("/ready/request_message_id", serde_json::json!(-1)),
            ("/ready/request_message_id", serde_json::json!(1.5)),
            ("/ready/request_message_id", serde_json::json!("1")),
            (
                "/ready/request_message_id",
                serde_json::json!(9_007_199_254_740_992_u64),
            ),
            ("/ready/response_message_id", serde_json::json!(1)),
            ("/sse_terminal", serde_json::json!(false)),
            ("/history/source", serde_json::json!("model_inferred")),
            ("/history/observed_at", serde_json::json!("invalid")),
            ("/history/cache_request/cache_version", serde_json::json!(0)),
            (
                "/history/cache_request/cache_reset_at",
                serde_json::json!("synthetic"),
            ),
            ("/history/cache_control", serde_json::json!("APPEND")),
            ("/history/chat_session_id", serde_json::json!("other-chat")),
            ("/history/messages/0/parent_id", serde_json::json!(0)),
            ("/history/messages/0/status", serde_json::json!("PENDING")),
            (
                "/history/messages/0/has_pending_fragment",
                serde_json::json!(true),
            ),
            ("/history/messages/1/message_id", serde_json::json!(3)),
            ("/history/messages/1/parent_id", serde_json::json!(0)),
            ("/history/messages/1/role", serde_json::json!("USER")),
            ("/history/messages/1/status", serde_json::json!("STREAMING")),
            (
                "/history/messages/1/has_pending_fragment",
                serde_json::json!(true),
            ),
            ("/history/current_message_id", serde_json::json!(1)),
            ("/history/current_message_id", serde_json::json!("2")),
        ] {
            let mut invalid = document["lifecycle"].clone();
            *invalid.pointer_mut(path).unwrap() = value;
            assert!(deepseek_lifecycle_chat_id(&invalid).is_none(), "{path}");
        }
        // Every object is closed and every non-optional property is required.
        for path in [
            "",
            "/creation",
            "/submission",
            "/ready",
            "/history",
            "/history/cache_request",
            "/history/messages/0",
            "/history/messages/1",
        ] {
            let mut invalid = document["lifecycle"].clone();
            invalid
                .pointer_mut(path)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("extra".into(), Value::Null);
            assert!(
                deepseek_lifecycle_chat_id(&invalid).is_none(),
                "extra at {path}"
            );
            for key in document["lifecycle"]
                .pointer(path)
                .unwrap()
                .as_object()
                .unwrap()
                .keys()
                .filter(|key| key.as_str() != "current_message_id")
            {
                let mut invalid = document["lifecycle"].clone();
                invalid
                    .pointer_mut(path)
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .remove(key);
                assert!(
                    deepseek_lifecycle_chat_id(&invalid).is_none(),
                    "missing {path}/{key}"
                );
            }
        }
        for messages in [
            serde_json::json!([]),
            serde_json::json!([document["lifecycle"]["history"]["messages"][0]]),
            serde_json::json!([
                document["lifecycle"]["history"]["messages"][0],
                document["lifecycle"]["history"]["messages"][1],
                document["lifecycle"]["history"]["messages"][1]
            ]),
        ] {
            let mut invalid = document["lifecycle"].clone();
            invalid["history"]["messages"] = messages;
            assert!(deepseek_lifecycle_chat_id(&invalid).is_none());
        }
    }

    #[test]
    fn deepseek_optional_history_fields_are_narrow_and_ids_are_js_safe() {
        for status in [Value::Null, serde_json::json!("FINISHED")] {
            let mut document = synthetic_deepseek_source();
            document["lifecycle"]["history"]["messages"][0]["quasi_status"] = status.clone();
            document["lifecycle"]["history"]["messages"][1]["quasi_status"] = status;
            assert!(complete(&document));
        }
        for status in [serde_json::json!("PENDING"), serde_json::json!(false)] {
            let mut document = synthetic_deepseek_source();
            document["lifecycle"]["history"]["messages"][1]["quasi_status"] = status;
            assert!(!complete(&document));
        }
        let mut document = synthetic_deepseek_source();
        document["lifecycle"]["history"]["current_message_id"] = Value::Null;
        assert!(complete(&document));
        document["lifecycle"]["history"]
            .as_object_mut()
            .unwrap()
            .remove("current_message_id");
        assert!(complete(&document));
        document["lifecycle"]["history"]["messages"]
            .as_array_mut()
            .unwrap()
            .reverse();
        assert!(complete(&document));
        for id in [0, 9_007_199_254_740_991_u64] {
            let mut document = synthetic_deepseek_source();
            for path in [
                "/messages/0/data/request_message_id",
                "/lifecycle/ready/request_message_id",
                "/lifecycle/history/messages/0/message_id",
                "/lifecycle/history/messages/1/parent_id",
            ] {
                *document.pointer_mut(path).unwrap() = serde_json::json!(id);
            }
            assert!(complete(&document));
        }
    }
}
