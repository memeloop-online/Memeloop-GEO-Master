import {
  isDeepSeekHistoryExplicitlyAbsent,
  verifyDeepSeekEmptyHistory,
  verifyDeepSeekFirstTurnHistory,
} from "./deepseek-history-evidence.mjs";
import {
  validDeepSeekSessionId,
  withDeepSeekDocument,
} from "./deepseek-web-transport.mjs";

const messageId = (value) =>
  (typeof value === "number" &&
    Number.isSafeInteger(value) &&
    value >= 0 &&
    !Object.is(value, -0)) ||
  (typeof value === "string" &&
    /^(?:0|[1-9]\d{0,15})$/u.test(value) &&
    Number.isSafeInteger(Number(value)) &&
    String(Number(value)) === value);

function observedHistory(envelope) {
  const observed = JSON.parse(envelope.body).data.biz_data;
  return {
    source: "browser_observed_history",
    observed_at: new Date().toISOString(),
    cache_request: { cache_version: null, cache_reset_at: null },
    cache_control: observed.cache_control,
    chat_session_id: observed.chat_session.id,
    messages: observed.chat_messages.map((message) => ({
      message_id: message.message_id,
      parent_id: message.parent_id,
      role: message.role,
      status: message.status,
      has_pending_fragment: message.has_pending_fragment,
      ...(Object.hasOwn(message, "quasi_status")
        ? { quasi_status: message.quasi_status }
        : {}),
    })),
    ...(Object.hasOwn(observed.chat_session, "current_message_id")
      ? { current_message_id: observed.chat_session.current_message_id }
      : {}),
  };
}

/**
 * Measurement remains bound to the caller's independently observed exact IDs.
 * No navigation, submission, retry, answer parsing or deletion authority.
 */
export async function readDeepSeekFirstTurnHistory(
  page,
  {
    conversationId,
    requestMessageId,
    responseMessageId,
    onFailureReason,
    allowEmptyHistory = false,
    ...options
  } = {},
) {
  const report = (reason) => {
    try {
      onFailureReason?.(reason);
    } catch {
      /* Diagnostics cannot affect evidence. */
    }
  };
  if (
    !validDeepSeekSessionId(conversationId) ||
    !messageId(requestMessageId) ||
    !messageId(responseMessageId) ||
    String(requestMessageId) === String(responseMessageId)
  ) {
    report("history_binding_mismatch");
    return null;
  }
  let reason;
  const result = await withDeepSeekDocument(
    page,
    {
      ...options,
      onFailureReason: (value) => {
        reason ??= value;
      },
    },
    async ({ request, active }) => {
      const envelope = await request("history", conversationId);
      if (!envelope || !active()) return null;
      const inventory =
        (allowEmptyHistory === true &&
          verifyDeepSeekEmptyHistory(envelope, conversationId)) ||
        verifyDeepSeekFirstTurnHistory(
          envelope,
          {
            conversationId,
            requestMessageId,
            responseMessageId,
          },
          {
            onFailureReason: (value) => {
              if (active()) reason ??= value;
            },
          },
        );
      return inventory
        ? { inventory, history: observedHistory(envelope) }
        : null;
    },
  );
  if (!result) report(reason ?? "completion_unverified");
  return result;
}

/**
 * Protocol projection only, not ownership or authorization. Candidate IDs are
 * taken from exactly two nodes and passed back through the raw-wire verifier,
 * including its canonical numeric-literal and complete graph checks.
 */
export function projectDeepSeekCleanupHistory(envelope, conversationId) {
  const unknown = { status: "unknown" };
  if (!validDeepSeekSessionId(conversationId)) return unknown;
  if (isDeepSeekHistoryExplicitlyAbsent(envelope)) return { status: "absent" };
  if (
    envelope?.status !== 200 ||
    typeof envelope.body !== "string" ||
    Buffer.byteLength(envelope.body, "utf8") > 750_000
  )
    return unknown;
  try {
    const empty = verifyDeepSeekEmptyHistory(envelope, conversationId);
    if (empty) return { status: "verified", inventory: empty };
    const messages = JSON.parse(envelope.body)?.data?.biz_data?.chat_messages;
    if (!Array.isArray(messages) || messages.length !== 2) return unknown;
    const user = messages.find((message) => message?.role === "USER");
    const assistant = messages.find((message) => message?.role === "ASSISTANT");
    const inventory = verifyDeepSeekFirstTurnHistory(envelope, {
      conversationId,
      requestMessageId: user?.message_id,
      responseMessageId: assistant?.message_id,
    });
    return inventory ? { status: "verified", inventory } : unknown;
  } catch {
    return unknown;
  }
}

/** Fresh exact inventory/strict absence/unknown; never returns message content. */
export async function readDeepSeekCleanupHistory(
  page,
  { conversationId, ...options } = {},
) {
  if (!validDeepSeekSessionId(conversationId)) return { status: "unknown" };
  return (
    (await withDeepSeekDocument(page, options, async ({ request }) =>
      projectDeepSeekCleanupHistory(
        await request("history", conversationId),
        conversationId,
      ),
    )) ?? { status: "unknown" }
  );
}
