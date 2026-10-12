import * as kimiConversationEvidence from "./kimi-conversation-evidence.mjs";

// Transport lifecycle proofs only, not measurement capabilities or semantic
// answer parsers. Selection belongs to the provider flow, never captured data.
const adapters = Object.freeze({
  kimi: kimiConversationEvidence,
});

function adapterFor(provider) {
  return typeof provider === "string" && Object.hasOwn(adapters, provider)
    ? adapters[provider]
    : null;
}

export function capturedNewConversationId(exchange, { provider } = {}) {
  return adapterFor(provider)?.capturedNewConversationId(exchange) ?? null;
}

export function capturedConversationCompletion(exchange, { provider } = {}) {
  return adapterFor(provider)?.capturedConversationCompletion(exchange) ?? null;
}

/**
 * The optional caller hook is an integration seam for an independently
 * durable resource ledger. A returned { durable: true } acknowledges only
 * the ownership receipt, never the original answer/evidence checkpoint.
 * Hook errors must not turn a completed provider submission into a retry.
 */
export async function reportCapturedConversation(
  exchange,
  purpose,
  onConversationCaptured,
  { provider } = {},
) {
  if (typeof onConversationCaptured !== "function") return "untracked";
  const conversationId = capturedNewConversationId(exchange, { provider });
  if (!conversationId) return "unidentified";
  try {
    const ack = await onConversationCaptured({
      provider,
      purpose,
      external_conversation_id: conversationId,
    });
    return ack?.durable === true ? "persisted" : "unpersisted";
  } catch {
    // Never log callback errors: they can contain private account context.
    return "unpersisted";
  }
}
