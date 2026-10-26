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
 * Narrow transport lifecycle evidence, not semantic answer extraction.
 * Incremental messages inherit identity fields, but every explicitly observed
 * status replaces the earlier status, including unknown/non-final values.
 * Neither model output nor a terminal Connect frame alone proves completion.
 */
export function capturedConversationCompletion(exchange) {
  if (exchange?.connect_json_terminal !== true) return null;
  const chatId = capturedNewConversationId(exchange);
  if (!chatId) return null;
  const messages = new Map();
  let precedingChat = null;
  for (const envelope of exchange.messages) {
    if (envelope.chat != null) precedingChat = envelope.chat.id;
    if (envelope.message == null) continue;
    // An omitted chat_id is scoped by a prior transport chat envelope, never
    // by a later envelope or inferred from the page/model response.
    if (precedingChat !== chatId) return null;
    const message = envelope.message;
    if (
      typeof message !== "object" ||
      Array.isArray(message) ||
      typeof message.id !== "string" ||
      !CHAT_ID.test(message.id)
    )
      return null;
    const state = messages.get(message.id) ?? {};
    if (Object.hasOwn(message, "role")) {
      const role = ["assistant", 3].includes(message.role)
        ? "assistant"
        : ["user", 2].includes(message.role)
          ? "user"
          : message.role === "system"
            ? "system"
            : null;
      if (!role || (state.role && state.role !== role)) return null;
      state.role = role;
    }
    if (Object.hasOwn(message, "chat_id")) {
      if (message.chat_id !== chatId) return null;
    }
    state.chatId = precedingChat;
    if (Object.hasOwn(message, "status")) state.status = message.status;
    messages.set(message.id, state);
  }
  const inventory = [];
  for (const [id, state] of messages) {
    if (!state.role || state.chatId !== chatId) return null;
    if (
      state.role === "assistant" &&
      !["COMPLETED", "MESSAGE_STATUS_COMPLETED", 2].includes(state.status)
    )
      return null;
    if (
      state.role === "system" &&
      ![
        "COMPLETED",
        "MESSAGE_STATUS_COMPLETED",
        2,
        "MESSAGE_STATUS_UNSPECIFIED",
      ].includes(state.status)
    )
      return null;
    inventory.push([id, state.role]);
  }
  return inventory.length <= 256 &&
    inventory.some(([, role]) => role === "user") &&
    inventory.some(([, role]) => role === "assistant")
    ? {
        outcome: "complete",
        message_inventory: inventory.sort(([left], [right]) =>
          left < right ? -1 : left > right ? 1 : 0,
        ),
      }
    : null;
}

// Recovery reads the exact first task captured on a newly created chat. The
// saved transport supplies every message identity; history cannot invent an
// owner or include a later user turn in that task's deletion inventory.
export function kimiRecoveryBinding(source, conversationId) {
  if (capturedNewConversationId(source) !== conversationId) return null;
  const messages = new Map();
  let precedingChat = null;
  for (const envelope of source.messages) {
    if (envelope.chat != null) precedingChat = envelope.chat.id;
    if (envelope.message == null) continue;
    const message = envelope.message;
    if (
      precedingChat !== conversationId ||
      typeof message !== "object" ||
      Array.isArray(message) ||
      typeof message.id !== "string" ||
      !CHAT_ID.test(message.id)
    )
      return null;
    let role = messages.get(message.id);
    if (Object.hasOwn(message, "role")) {
      const observed = ["assistant", 3].includes(message.role)
        ? "assistant"
        : ["user", 2].includes(message.role)
          ? "user"
          : message.role === "system"
            ? "system"
            : null;
      if (!observed || (role && role !== observed)) return null;
      role = observed;
    }
    messages.set(message.id, role);
  }
  const inventory = [...messages].sort(([left], [right]) =>
    left < right ? -1 : left > right ? 1 : 0,
  );
  return inventory.length <= 256 &&
    inventory.every(([, role]) => role) &&
    inventory.filter(([, role]) => role === "user").length === 1 &&
    inventory.some(([, role]) => role === "assistant")
    ? { message_inventory: inventory }
    : null;
}

export function recoveredKimiCompletion(binding, history, conversationId) {
  if (
    !binding ||
    history?.status !== "recovered" ||
    history.external_conversation_id !== conversationId ||
    history.chat?.chat?.id !== conversationId ||
    !Array.isArray(history.message_pages) ||
    history.message_pages.length === 0
  )
    return null;
  const expected = new Map(binding.message_inventory);
  const seen = new Set();
  for (const page of history.message_pages) {
    if (!Array.isArray(page?.messages)) return null;
    for (const message of page.messages) {
      const role = ["assistant", 3].includes(message?.role)
        ? "assistant"
        : ["user", 2].includes(message?.role)
          ? "user"
          : message?.role === "system"
            ? "system"
            : null;
      if (
        !role ||
        seen.has(message.id) ||
        expected.get(message.id) !== role ||
        (message.chat_id !== undefined && message.chat_id !== conversationId) ||
        (role === "assistant" &&
          !["COMPLETED", "MESSAGE_STATUS_COMPLETED", 2].includes(
            message.status,
          )) ||
        (role === "system" &&
          ![
            "COMPLETED",
            "MESSAGE_STATUS_COMPLETED",
            2,
            "MESSAGE_STATUS_UNSPECIFIED",
          ].includes(message.status))
      )
        return null;
      seen.add(message.id);
    }
  }
  return seen.size === expected.size
    ? { outcome: "complete", message_inventory: binding.message_inventory }
    : null;
}
