// Internal authenticated-runner metadata, never an AI-generated ownership claim.
// Completion identifies a finished graph; it does not authorize deletion.
import { sourceRedaction } from "./observation-source-redaction.mjs";

const object = (value) =>
  value !== null && typeof value === "object" && !Array.isArray(value);
const closed = (value, required, optional = []) =>
  object(value) &&
  required.every((key) => Object.hasOwn(value, key)) &&
  Object.keys(value).every((key) => [...required, ...optional].includes(key));
const chatId = (value) =>
  typeof value === "string" && /^[A-Za-z0-9_-]{1,128}$/u.test(value);
const model = (value) =>
  typeof value === "string" && /^[A-Za-z0-9_.-]{1,128}$/u.test(value);
const messageId = (value) =>
  Number.isSafeInteger(value) && value >= 0 && !Object.is(value, -0);

function timestamp(value) {
  if (typeof value !== "string" || value.length > 64) return false;
  const parts =
    /^(\d{4})-(\d{2})-(\d{2})[Tt](\d{2}):(\d{2}):(\d{2})(?:\.\d+)?(?:[Zz]|([+-])(\d{2}):(\d{2}))$/u.exec(
      value,
    );
  if (!parts) return false;
  const [, year, month, day, hour, minute, second, , zoneHour, zoneMinute] =
    parts;
  const leap = +year % 4 === 0 && (+year % 100 !== 0 || +year % 400 === 0);
  const days = [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
  return (
    +month >= 1 &&
    +month <= 12 &&
    +day >= 1 &&
    +day <= days[+month - 1] &&
    +hour <= 23 &&
    +minute <= 59 &&
    +second <= 60 &&
    (zoneHour === undefined || (+zoneHour <= 23 && +zoneMinute <= 59))
  );
}

function validCreationSubmission({ creation, submission }) {
  return (
    closed(creation, ["source", "chat_session_id"]) &&
    creation.source === "browser_observed_create" &&
    chatId(creation.chat_session_id) &&
    closed(submission, [
      "chat_session_id",
      "parent_message_id",
      "model_type",
      "prompt_sha256",
      "search_enabled",
      "thinking_enabled",
    ]) &&
    submission.chat_session_id === creation.chat_session_id &&
    submission.parent_message_id === null &&
    model(submission.model_type) &&
    typeof submission.prompt_sha256 === "string" &&
    /^[a-f0-9]{64}$/u.test(submission.prompt_sha256) &&
    submission.search_enabled === true &&
    submission.thinking_enabled === false
  );
}

export function validDeepSeekOwnership(ownership) {
  return (
    closed(ownership, [
      "schema_version",
      "provider",
      "creation",
      "submission",
    ]) &&
    ownership.schema_version === "geo.deepseek.ownership.v1" &&
    ownership.provider === "deepseek" &&
    validCreationSubmission(ownership)
  );
}

export function validDeepSeekLifecycle(lifecycle) {
  if (
    !closed(lifecycle, [
      "schema_version",
      "provider",
      "creation",
      "submission",
      "ready",
      "sse_terminal",
      "history",
    ]) ||
    lifecycle.schema_version !== "geo.deepseek.lifecycle.v1" ||
    lifecycle.provider !== "deepseek" ||
    lifecycle.sse_terminal !== true
  )
    return false;
  const { creation, ready, history } = lifecycle;
  if (
    !validCreationSubmission(lifecycle) ||
    !closed(ready, ["request_message_id", "response_message_id"]) ||
    !messageId(ready.request_message_id) ||
    !messageId(ready.response_message_id) ||
    ready.request_message_id === ready.response_message_id
  )
    return false;
  if (
    !closed(
      history,
      [
        "source",
        "observed_at",
        "cache_request",
        "cache_control",
        "chat_session_id",
        "messages",
      ],
      ["current_message_id"],
    ) ||
    history.source !== "browser_observed_history" ||
    !timestamp(history.observed_at) ||
    !closed(history.cache_request, ["cache_version", "cache_reset_at"]) ||
    history.cache_request.cache_version !== null ||
    history.cache_request.cache_reset_at !== null ||
    history.cache_control !== "REPLACE" ||
    history.chat_session_id !== creation.chat_session_id ||
    (Object.hasOwn(history, "current_message_id") &&
      history.current_message_id !== null &&
      history.current_message_id !== ready.response_message_id) ||
    !Array.isArray(history.messages) ||
    history.messages.length !== 2
  )
    return false;
  let user = false;
  let assistant = false;
  for (const message of history.messages) {
    if (
      !closed(
        message,
        ["message_id", "parent_id", "role", "status", "has_pending_fragment"],
        ["quasi_status"],
      ) ||
      !messageId(message.message_id) ||
      message.status !== "FINISHED" ||
      message.has_pending_fragment !== false ||
      (Object.hasOwn(message, "quasi_status") &&
        message.quasi_status !== null &&
        message.quasi_status !== "FINISHED")
    )
      return false;
    if (
      message.role === "USER" &&
      message.message_id === ready.request_message_id &&
      message.parent_id === null &&
      !user
    )
      user = true;
    else if (
      message.role === "ASSISTANT" &&
      message.message_id === ready.response_message_id &&
      message.parent_id === ready.request_message_id &&
      !assistant
    )
      assistant = true;
    else return false;
  }
  return user && assistant;
}

// Presence is meaningful: malformed or conflicting metadata cannot fall back
// to another proof. Legacy lifecycle-only snapshots retain their v1 behavior.
export function validDeepSeekEvidence(exchange) {
  if (!object(exchange)) return false;
  const hasOwnership = Object.hasOwn(exchange, "ownership");
  const hasLifecycle = Object.hasOwn(exchange, "lifecycle");
  if (
    (!hasOwnership && !hasLifecycle) ||
    (hasOwnership && !validDeepSeekOwnership(exchange.ownership)) ||
    (hasLifecycle && !validDeepSeekLifecycle(exchange.lifecycle))
  )
    return false;
  if (!hasOwnership) return true;
  const { ownership, lifecycle } = exchange;
  if (
    hasLifecycle &&
    ["creation", "submission"].some((section) =>
      Object.keys(ownership[section]).some(
        (key) => ownership[section][key] !== lifecycle[section][key],
      ),
    )
  )
    return false;
  return (
    Array.isArray(exchange.messages) &&
    exchange.messages.every(
      (message) =>
        message?.event !== "ready" ||
        (object(message.data) &&
          (message.data.model_type == null ||
            message.data.model_type === ownership.submission.model_type)),
    )
  );
}

export function capturedNewConversationId(exchange) {
  if (!validDeepSeekEvidence(exchange)) return null;
  return (exchange.ownership ?? exchange.lifecycle).creation.chat_session_id;
}

function completeStream(exchange, readyIds, expectedModel) {
  if (!Array.isArray(exchange.messages) || exchange.messages.length > 32_768)
    return false;
  let omittedPaths = new Set();
  if (Object.hasOwn(exchange, "source_redaction")) {
    try {
      const manifest = sourceRedaction(exchange.source_redaction, exchange);
      omittedPaths = new Set(manifest.omitted_paths);
      if (omittedPaths.size !== manifest.omitted_paths.length) return null;
    } catch {
      return null;
    }
  }
  let ready = false;
  let finished = false;
  let closeSeen = false;
  for (const [index, envelope] of exchange.messages.entries()) {
    if (
      !object(envelope) ||
      typeof envelope.event !== "string" ||
      (envelope.event !== "finish" && !object(envelope.data))
    )
      return null;
    if (envelope.event === "ready") {
      if (
        ready ||
        finished ||
        closeSeen ||
        !object(envelope.data) ||
        envelope.data.request_message_id !== readyIds.request_message_id ||
        envelope.data.response_message_id !== readyIds.response_message_id ||
        (envelope.data.model_type !== undefined &&
          envelope.data.model_type !== null &&
          envelope.data.model_type !== expectedModel)
      )
        return null;
      ready = true;
    } else if (envelope.event === "finish") {
      // The omitted payload remains opaque; only its observed event framing
      // and an exact, validated omission declaration survive projection.
      const finishPayload =
        typeof envelope.raw_data === "string" ||
        (!Object.hasOwn(envelope, "raw_data") &&
          omittedPaths.has(`/messages/${index}/raw_data`));
      if (!ready || finished || !finishPayload) return null;
      finished = true;
    } else if (envelope.event === "close") {
      if (
        !ready ||
        closeSeen ||
        (Object.hasOwn(envelope.data, "click_behavior") &&
          envelope.data.click_behavior !== "retry" &&
          envelope.data.click_behavior !== "none")
      )
        return null;
      closeSeen = true;
    } else if (
      (!envelope.event ||
        envelope.event === "message" ||
        envelope.event === "delta") &&
      (!ready || finished || closeSeen)
    )
      return null;
    // Do not interpret delta bodies, answer text, citations or finish payload.
  }
  // Old partial/eof records are immutable evidence, not upgrade candidates.
  const completeClose =
    closeSeen &&
    closed(exchange.capture_status, [
      "schema_version",
      "transport",
      "termination",
    ]) &&
    exchange.capture_status.schema_version ===
      "geo.observation.capture-status.v1" &&
    exchange.capture_status.transport === "complete" &&
    exchange.capture_status.termination === "complete";
  return ready && (finished || completeClose);
}

export function capturedConversationCompletion(exchange) {
  if (
    !validDeepSeekEvidence(exchange) ||
    !validDeepSeekLifecycle(exchange?.lifecycle) ||
    !completeStream(
      exchange,
      exchange.lifecycle.ready,
      exchange.lifecycle.submission.model_type,
    )
  )
    return null;
  return {
    outcome: "complete",
    message_inventory: exchange.lifecycle.history.messages
      .map((message) => [
        String(message.message_id),
        message.role === "USER" ? "user" : "assistant",
      ])
      .sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0)),
  };
}

/** Saved-source strategy binding. Fresh history cannot supply missing stream
 * IDs, ownership or terminal events from the original request. */
export function deepSeekRecoveryBinding(source, conversationId) {
  if (
    !validDeepSeekEvidence(source) ||
    Object.hasOwn(source, "lifecycle") ||
    source.ownership.creation.chat_session_id !== conversationId ||
    !Array.isArray(source.messages)
  )
    return null;
  const ready = source.messages.filter((message) => message?.event === "ready");
  if (ready.length !== 1) return null;
  const ids = ready[0].data;
  if (
    !object(ids) ||
    !messageId(ids.request_message_id) ||
    !messageId(ids.response_message_id) ||
    ids.request_message_id === ids.response_message_id ||
    !completeStream(source, ids, source.ownership.submission.model_type)
  )
    return null;
  return {
    requestMessageId: ids.request_message_id,
    responseMessageId: ids.response_message_id,
  };
}

export function recoveredDeepSeekCompletion(source, history, conversationId) {
  const binding = deepSeekRecoveryBinding(source, conversationId);
  if (!binding) return null;
  const messages = source.messages;
  if (
    messages.length === 3 &&
    messages[0].event === "ready" &&
    messages[1].event === "hint" &&
    messages[1].data?.type === "error" &&
    messages[1].data?.finish_reason === "backend_err" &&
    messages[1].data?.clear_response === true &&
    messages[2].event === "close" &&
    source.capture_status?.transport === "complete" &&
    source.capture_status?.termination === "complete" &&
    closed(history, [
      "source",
      "observed_at",
      "cache_request",
      "cache_control",
      "chat_session_id",
      "messages",
      "current_message_id",
    ]) &&
    history.source === "browser_observed_history" &&
    timestamp(history.observed_at) &&
    closed(history.cache_request, ["cache_version", "cache_reset_at"]) &&
    history.cache_request.cache_version === null &&
    history.cache_request.cache_reset_at === null &&
    history.cache_control === "REPLACE" &&
    history.chat_session_id === conversationId &&
    history.current_message_id === null &&
    Array.isArray(history.messages) &&
    history.messages.length === 0
  )
    return { outcome: "cleared_failure", message_inventory: [] };
  return capturedConversationCompletion({
    ...source,
    lifecycle: {
      schema_version: "geo.deepseek.lifecycle.v1",
      provider: "deepseek",
      creation: source.ownership.creation,
      submission: source.ownership.submission,
      ready: {
        request_message_id: binding.requestMessageId,
        response_message_id: binding.responseMessageId,
      },
      sse_terminal: true,
      history,
    },
  });
}
