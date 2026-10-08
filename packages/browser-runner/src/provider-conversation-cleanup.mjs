import { probeKimiAccount } from "./adapters.mjs";

const ORIGIN = "https://www.kimi.com";
const CHAT_SERVICE = "/apiv2/kimi.gateway.chat.v1.ChatService/";
const GET_CHAT = `${CHAT_SERVICE}GetChat`;
const LIST_MESSAGES = `${CHAT_SERVICE}ListMessages`;
const DELETE_CHAT = `${CHAT_SERVICE}DeleteChat`;
const CHAT_ID = /^[\w-]{1,128}$/u;
const MAX_RESPONSE_BYTES = 256_000;
const PAGE_SIZE = 100;
const MAX_PAGES = 10;

function validId(value) {
  return typeof value === "string" && CHAT_ID.test(value);
}

function validOrigin(value) {
  try {
    const url = new URL(value);
    return url.origin === value && ["https:", "http:"].includes(url.protocol);
  } catch {
    return false;
  }
}

function chatIdentity(data) {
  return validId(data?.chat?.id) ? data.chat.id : null;
}

function generating(value) {
  if (!value || typeof value !== "object") return false;
  const states = [
    value.status,
    value.state,
    value.chat?.status,
    value.chat?.state,
  ];
  return states.some(
    (state) =>
      typeof state === "string" &&
      /^(generating|streaming|running|pending|in_progress)$/iu.test(state),
  );
}

// Every request is constructed and executed inside the existing Playwright
// page. No token, arbitrary URL, or browser navigation is exposed to Node.
async function connect(page, { trustedOrigin, path, body, timeoutMs }) {
  return page.evaluate(
    async ({ origin, path, body, timeoutMs, limit }) => {
      if (location.origin !== origin) return { kind: "wrong_origin" };
      const token = localStorage.getItem("access_token");
      const refresh = localStorage.getItem("refresh_token");
      if (!token || !refresh) return { kind: "reauth_required" };
      try {
        const response = await fetch(path, {
          method: "POST",
          credentials: "same-origin",
          redirect: "error",
          signal: AbortSignal.timeout(timeoutMs),
          headers: {
            Authorization: `Bearer ${token}`,
            "Content-Type": "application/json",
            "Connect-Protocol-Version": "1",
            "x-msh-platform": "web",
          },
          body: JSON.stringify(body),
        });
        if (
          response.url !== `${origin}${path}` ||
          !/application\/json/i.test(response.headers.get("content-type") ?? "")
        ) {
          return { kind: "invalid_response" };
        }
        if (response.status === 401 || response.status === 403)
          return { kind: "reauth_required" };
        if (!response.ok)
          return { kind: "http_error", status: response.status };
        const reader = response.body?.getReader();
        if (!reader) return { kind: "invalid_response" };
        const decoder = new TextDecoder();
        let text = "";
        let bytes = 0;
        while (true) {
          const { done, value } = await reader.read();
          if (done) break;
          bytes += value.byteLength;
          if (bytes > limit) {
            await reader.cancel();
            return { kind: "too_large" };
          }
          text += decoder.decode(value, { stream: true });
        }
        text += decoder.decode();
        return { kind: "ok", data: JSON.parse(text) };
      } catch {
        return { kind: "transport_unknown" };
      }
    },
    {
      origin: trustedOrigin,
      path,
      body,
      timeoutMs,
      limit: MAX_RESPONSE_BYTES,
    },
  );
}

function optionsValid({ trustedOrigin, expectedUserId, chatId, timeoutMs }) {
  return (
    validOrigin(trustedOrigin) &&
    validId(expectedUserId) &&
    validId(chatId) &&
    Number.isInteger(timeoutMs) &&
    timeoutMs >= 1 &&
    timeoutMs <= 30_000
  );
}

async function inspect(page, options) {
  const identity = await probeKimiAccount(page, {
    trustedOrigin: options.trustedOrigin,
  });
  if (!identity) return { status: "reauth_required" };
  if (identity.platform_account_id !== options.expectedUserId)
    return { status: "retained", reason: "account_mismatch" };
  const response = await connect(page, {
    ...options,
    path: GET_CHAT,
    body: { chat_id: options.chatId },
  });
  if (response.kind !== "ok")
    return {
      status:
        response.kind === "reauth_required" ? "reauth_required" : "unknown",
      reason: response.kind,
    };
  if (chatIdentity(response.data) !== options.chatId)
    return { status: "unknown", reason: "chat_mismatch" };
  if (generating(response.data))
    return { status: "retained", reason: "generating" };
  return { status: "matched", chat: response.data };
}

/**
 * Passive read of a previously identified conversation. It never visits the
 * history UI or calls ResumeChat, which can continue generation. The returned
 * raw response is evidence to persist, not a durability receipt. A bounded or
 * malformed pagination sequence remains retained and cannot authorize delete.
 */
export async function recoverKimiConversation(
  page,
  {
    trustedOrigin = ORIGIN,
    expectedUserId,
    chatId,
    timeoutMs = 12_000,
    maxPages = MAX_PAGES,
  } = {},
) {
  const options = { trustedOrigin, expectedUserId, chatId, timeoutMs };
  if (
    !optionsValid(options) ||
    !Number.isInteger(maxPages) ||
    maxPages < 1 ||
    maxPages > MAX_PAGES
  )
    return { status: "retained", reason: "invalid_scope" };
  try {
    const result = await inspect(page, options);
    if (result.status !== "matched") return result;
    const pages = [];
    const tokens = new Set();
    let pageToken = "";
    for (let index = 0; index < maxPages; index++) {
      const response = await connect(page, {
        ...options,
        path: LIST_MESSAGES,
        body: { chat_id: chatId, page_size: PAGE_SIZE, page_token: pageToken },
      });
      if (response.kind !== "ok")
        return {
          status:
            response.kind === "reauth_required"
              ? "reauth_required"
              : "retained",
          reason: response.kind,
        };
      const data = response.data;
      if (
        !Array.isArray(data?.messages) ||
        data.messages.length > PAGE_SIZE ||
        (data.chat_id != null && data.chat_id !== chatId) ||
        generating(data) ||
        data.messages.some(
          (message) =>
            !message ||
            typeof message !== "object" ||
            (message.chat_id != null && message.chat_id !== chatId) ||
            generating(message),
        )
      )
        return { status: "retained", reason: "unverified_messages" };
      pages.push(data);
      const next = data.next_page_token ?? "";
      if (next === "" && data.has_more !== true) {
        return {
          status: "recovered",
          external_conversation_id: chatId,
          chat: result.chat,
          message_pages: pages,
        };
      }
      if (next === "")
        return { status: "retained", reason: "pagination_incomplete" };
      if (
        typeof next !== "string" ||
        next.length > 512 ||
        tokens.has(next) ||
        index + 1 >= maxPages
      )
        return { status: "retained", reason: "pagination_incomplete" };
      tokens.add(next);
      pageToken = next;
    }
    return { status: "retained", reason: "pagination_incomplete" };
  } catch {
    return { status: "unknown", reason: "transport_unknown" };
  }
}

/**
 * One exact delete attempt, never invoked by recovery. The trusted caller
 * must independently validate persisted ownership, durable original evidence,
 * and absence of active task references before returning an authorization
 * bound to this exact account/chat. A captured ownership receipt alone does
 * not satisfy this callback. The adapter cannot establish database durability.
 */
export async function deleteKimiConversation(
  page,
  {
    trustedOrigin = ORIGIN,
    expectedUserId,
    chatId,
    timeoutMs = 12_000,
    authorizeDeletion,
  } = {},
) {
  const options = { trustedOrigin, expectedUserId, chatId, timeoutMs };
  if (!optionsValid(options))
    return { status: "retained", reason: "invalid_scope" };
  if (typeof authorizeDeletion !== "function")
    return { status: "retained", reason: "authorization_required" };
  try {
    const first = await inspect(page, options);
    if (first.status !== "matched") return first;
    let authority;
    try {
      authority = await authorizeDeletion({
        provider: "kimi",
        platform_account_id: expectedUserId,
        external_conversation_id: chatId,
      });
    } catch {
      return { status: "retained", reason: "authorization_required" };
    }
    if (
      authority?.authorized !== true ||
      authority.platform_account_id !== expectedUserId ||
      authority.external_conversation_id !== chatId
    )
      return { status: "retained", reason: "authorization_required" };
    // Re-check after the awaited ledger authorization: the account or
    // conversation may have changed while the caller consulted durable state.
    const second = await inspect(page, options);
    if (second.status !== "matched") return second;
    const response = await connect(page, {
      ...options,
      path: DELETE_CHAT,
      body: { chat_id: chatId },
    });
    if (response.kind !== "ok")
      return {
        status:
          response.kind === "reauth_required" ? "reauth_required" : "unknown",
        reason: response.kind,
      };
    if (response.data?.chat_id !== chatId)
      return { status: "unknown", reason: "unverified_delete_response" };
    return { status: "deleted", external_conversation_id: chatId };
  } catch {
    // The delete may have reached the provider. Never retry automatically.
    return { status: "unknown", reason: "transport_unknown" };
  }
}
