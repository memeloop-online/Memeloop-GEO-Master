const MAX_BYTES = 750_000;
const SESSION_ID = /^[\w-]{1,128}$/u;
const DECIMAL_ID = /^(?:0|[1-9]\d{0,15})$/u;
const object = (value) =>
  value !== null && typeof value === "object" && !Array.isArray(value);

function numericId(value) {
  return typeof value === "number" &&
    Number.isSafeInteger(value) &&
    value >= 0 &&
    !Object.is(value, -0)
    ? String(value)
    : null;
}

function expectedId(value) {
  if (typeof value === "number") return numericId(value);
  return typeof value === "string" &&
    DECIMAL_ID.test(value) &&
    numericId(Number(value)) === value
    ? value
    : null;
}

function parseEnvelope(envelope) {
  if (
    !object(envelope) ||
    envelope.status !== 200 ||
    typeof envelope.body !== "string" ||
    envelope.body.length > MAX_BYTES ||
    Buffer.byteLength(envelope.body, "utf8") > MAX_BYTES
  )
    return null;
  try {
    const invalidIds = new WeakSet();
    const value = JSON.parse(envelope.body, function (key, entry, context) {
      if (
        ["message_id", "parent_id", "current_message_id"].includes(key) &&
        typeof entry === "number" &&
        (!DECIMAL_ID.test(context?.source ?? "") ||
          numericId(entry) !== context?.source)
      )
        invalidIds.add(this);
      return entry;
    });
    if (!object(value) || value.code !== 0 || !object(value.data)) return null;
    return { data: value.data, invalidIds };
  } catch {
    // Raw response content and parser diagnostics never leave this boundary.
    return null;
  }
}

/**
 * Verify only a complete, fresh first-turn transport inventory. Caller must
 * independently bind this response to the authenticated account, exact history
 * request (without an incremental cache), and owned creation/completion request.
 * This grants no deletion authority and proves neither answer quality nor local
 * durability. Body must be raw JSON, bounded to 750,000 UTF-8 bytes.
 *
 * Conservative policy: actual FINISHED status, absent/null/FINISHED quasi status,
 * and explicitly false pending flag. Missing pending flags remain unverified.
 * Numeric wire IDs must use canonical nonnegative safe-integer JSON literals.
 * Fragment bodies, citations and session metadata are never returned.
 */
export function verifyDeepSeekFirstTurnHistory(envelope, expected) {
  if (
    !object(expected) ||
    typeof expected.conversationId !== "string" ||
    !SESSION_ID.test(expected.conversationId)
  )
    return null;
  const requestId = expectedId(expected.requestMessageId);
  const responseId = expectedId(expected.responseMessageId);
  if (requestId === null || responseId === null || requestId === responseId)
    return null;
  const parsed = parseEnvelope(envelope);
  const history = parsed?.data?.biz_data;
  if (
    parsed?.data?.biz_code !== 0 ||
    !object(history) ||
    !object(history.chat_session) ||
    history.chat_session.id !== expected.conversationId ||
    parsed.invalidIds.has(history.chat_session) ||
    (history.chat_session.current_message_id != null &&
      numericId(history.chat_session.current_message_id) !== responseId) ||
    history.cache_control !== "REPLACE" ||
    !Array.isArray(history.chat_messages) ||
    history.chat_messages.length !== 2
  )
    return null;
  const nodes = new Map();
  for (const message of history.chat_messages) {
    if (
      !object(message) ||
      parsed.invalidIds.has(message) ||
      message.status !== "FINISHED" ||
      ![undefined, null, "FINISHED"].includes(message.quasi_status) ||
      message.has_pending_fragment !== false ||
      !Array.isArray(message.fragments)
    )
      return null;
    const id = numericId(message.message_id);
    if (id === null || nodes.has(id)) return null;
    nodes.set(id, message);
  }
  const user = nodes.get(requestId);
  const assistant = nodes.get(responseId);
  if (
    user?.role !== "USER" ||
    user.parent_id !== null ||
    assistant?.role !== "ASSISTANT" ||
    numericId(assistant.parent_id) !== requestId
  )
    return null;
  return {
    conversationId: expected.conversationId,
    messages: [
      { id: requestId, role: "user" },
      { id: responseId, role: "assistant" },
    ],
  };
}

/**
 * Only this explicit business response is absence evidence. Empty/missing
 * history, HTTP errors and biz_code 5 are not absence. Caller must independently
 * prove which authenticated history request produced this envelope.
 */
export function isDeepSeekHistoryExplicitlyAbsent(envelope) {
  return parseEnvelope(envelope)?.data.biz_code === 1;
}
