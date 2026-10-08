// A cleanup candidate must be identified by the response to this exact,
// freshly submitted request. A page URL, title or an AI-extracted ID is not
// evidence of ownership. This is only an identification receipt; it says
// nothing about local evidence durability or permission to delete.
const CHAT_ID = /^[\w-]{1,128}$/u;

export function capturedNewConversationId(exchange) {
  if (!Array.isArray(exchange?.messages)) return null;
  let chatId = null;
  let sawChatEnvelope = false;
  for (const envelope of exchange.messages) {
    if (!envelope || typeof envelope !== "object" || Array.isArray(envelope))
      return null;
    if (envelope.error != null) return null;
    if (
      ["chat", "message", "block", "ref", "done"].filter(
        (key) => envelope[key] != null,
      ).length > 1
    )
      return null;
    if (envelope.chat != null) {
      const id = envelope.chat?.id;
      if (typeof id !== "string" || !CHAT_ID.test(id)) return null;
      if (chatId && chatId !== id) return null;
      chatId = id;
      sawChatEnvelope = true;
    }
    if (envelope.message?.chat_id != null) {
      const id = envelope.message.chat_id;
      if (typeof id !== "string" || !CHAT_ID.test(id)) return null;
      if (chatId && chatId !== id) return null;
      chatId = id;
    }
  }
  return sawChatEnvelope ? chatId : null;
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
) {
  if (typeof onConversationCaptured !== "function") return "untracked";
  const conversationId = capturedNewConversationId(exchange);
  if (!conversationId) return "unidentified";
  try {
    const ack = await onConversationCaptured({
      provider: "kimi",
      purpose,
      external_conversation_id: conversationId,
    });
    return ack?.durable === true ? "persisted" : "unpersisted";
  } catch {
    // Never log callback errors: they can contain private account context.
    return "unpersisted";
  }
}
