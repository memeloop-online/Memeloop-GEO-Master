import { probeKimiAccount } from "./adapters.mjs";
import { createHash } from "node:crypto";

const ORIGIN = "https://www.kimi.com";
const CHAT_SERVICE = "/apiv2/kimi.gateway.chat.v1.ChatService/";
const GET_CHAT = `${CHAT_SERVICE}GetChat`;
const LIST_MESSAGES = `${CHAT_SERVICE}ListMessages`;
const DELETE_CHAT = `${CHAT_SERVICE}DeleteChat`;
const CHAT_ID = /^[\w-]{1,128}$/u;
const MAX_RESPONSE_BYTES = 256_000;
// History includes full message bodies even though cleanup compares only IDs
// and roles. Support the same byte budget as persisted observation sources
// (MAX_OBSERVATION_SOURCE_BYTES); never truncate a page to obtain an inventory.
const MAX_MESSAGE_RESPONSE_BYTES = 750_000;
const PAGE_SIZE = 100;
const MAX_PAGES = 10;
const DELETE_CLOSE_MARGIN_MS = 2_000;
const INVENTORY_SHA256 = /^[0-9a-f]{64}$/;

const DIAGNOSTIC_STAGES = new Set([
  "scope",
  "identity",
  "inspection",
  "authorization",
  "messages",
  "inventory",
  "delete",
  "deadline",
  "runner",
]);
const DIAGNOSTIC_CODES = new Set([
  "invalid_scope",
  "unsupported_platform",
  "reauth_required",
  "account_mismatch",
  "wrong_origin",
  "invalid_response",
  "http_error",
  "too_large",
  "transport_unknown",
  "chat_mismatch",
  "generating",
  "authorization_required",
  "authorization_expired",
  "unverified_messages",
  "pagination_incomplete",
  "message_inventory_mismatch",
  "unverified_delete_response",
  "deadline_exceeded",
]);

// Reconstruct only the closed diagnostic vocabulary. Never forward response
// bodies, exception text, account identifiers, or arbitrary provider reasons.
export function cleanupDiagnostic(value) {
  if (
    !value ||
    typeof value !== "object" ||
    Array.isArray(value) ||
    Object.keys(value).length !== 2 ||
    !Object.hasOwn(value, "stage") ||
    !Object.hasOwn(value, "code") ||
    !DIAGNOSTIC_STAGES.has(value.stage) ||
    !DIAGNOSTIC_CODES.has(value.code)
  )
    return undefined;
  return { stage: value.stage, code: value.code };
}

function failure(status, stage, reason) {
  const diagnostic = cleanupDiagnostic({ stage, code: reason });
  return { status, reason, ...(diagnostic ? { diagnostic } : {}) };
}

function retainedMessageInventory(pages) {
  const inventory = [];
  const seen = new Set();
  for (const page of pages) {
    for (const message of page.messages) {
      const role =
        message.role === "user" || message.role === 2
          ? "user"
          : message.role === "assistant" || message.role === 3
            ? "assistant"
            : message.role === "system"
              ? "system"
              : null;
      if (
        !validId(message.id) ||
        seen.has(message.id) ||
        role === null ||
        ((role === "assistant" || role === "system") &&
          !["COMPLETED", "MESSAGE_STATUS_COMPLETED", 2].includes(
            message.status,
          ))
      )
        return null;
      seen.add(message.id);
      inventory.push([message.id, role]);
    }
  }
  if (
    !inventory.some(([, role]) => role === "user") ||
    !inventory.some(([, role]) => role === "assistant")
  )
    return null;
  inventory.sort(([left], [right]) =>
    left < right ? -1 : left > right ? 1 : 0,
  );
  return createHash("sha256").update(JSON.stringify(inventory)).digest("hex");
}

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
      /^(generating|streaming|running|pending|in_progress|MESSAGE_STATUS_GENERATING)$/iu.test(
        state,
      ),
  );
}

// Every request is constructed and executed inside the existing Playwright
// page. No token, arbitrary URL, or browser navigation is exposed to Node.
async function connect(
  page,
  { trustedOrigin, path, body, timeoutMs, deleteNotAfterMs },
) {
  return page.evaluate(
    async ({
      origin,
      path,
      body,
      timeoutMs,
      limit,
      deleteNotAfterMs,
      closeMarginMs,
    }) => {
      if (location.origin !== origin) return { kind: "wrong_origin" };
      const token = localStorage.getItem("access_token");
      const refresh = localStorage.getItem("refresh_token");
      if (!token || !refresh) return { kind: "reauth_required" };
      try {
        // Include dispatch latency in the absolute grant budget. Do not send
        // when the full request timeout plus lock-release margin cannot fit.
        if (deleteNotAfterMs !== undefined) {
          const remaining = deleteNotAfterMs - Date.now() - closeMarginMs;
          if (remaining < timeoutMs) return { kind: "authorization_expired" };
          timeoutMs = Math.min(timeoutMs, remaining);
        }
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
      limit:
        path === LIST_MESSAGES
          ? MAX_MESSAGE_RESPONSE_BYTES
          : MAX_RESPONSE_BYTES,
      deleteNotAfterMs,
      closeMarginMs: DELETE_CLOSE_MARGIN_MS,
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
  if (!identity)
    return failure("reauth_required", "identity", "reauth_required");
  if (identity.platform_account_id !== options.expectedUserId)
    return failure("retained", "identity", "account_mismatch");
  const response = await connect(page, {
    ...options,
    path: GET_CHAT,
    body: { chat_id: options.chatId },
  });
  if (response.kind !== "ok")
    return failure(
      response.kind === "reauth_required" ? "reauth_required" : "unknown",
      "inspection",
      response.kind,
    );
  if (chatIdentity(response.data) !== options.chatId)
    return failure("unknown", "inspection", "chat_mismatch");
  if (generating(response.data))
    return failure("retained", "inspection", "generating");
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
    return failure("retained", "scope", "invalid_scope");
  let stage = "inspection";
  try {
    const result = await inspect(page, options);
    if (result.status !== "matched") return result;
    const pages = [];
    stage = "messages";
    const tokens = new Set();
    let pageToken = "";
    for (let index = 0; index < maxPages; index++) {
      const response = await connect(page, {
        ...options,
        path: LIST_MESSAGES,
        body: { chat_id: chatId, page_size: PAGE_SIZE, page_token: pageToken },
      });
      if (response.kind !== "ok")
        return failure(
          response.kind === "reauth_required" ? "reauth_required" : "retained",
          "messages",
          response.kind,
        );
      const data = response.data;
      if (
        !Array.isArray(data?.messages) ||
        data.messages.length > PAGE_SIZE ||
        (data.has_more !== undefined && typeof data.has_more !== "boolean") ||
        (data.next_page_token !== undefined &&
          typeof data.next_page_token !== "string") ||
        (data.chat_id !== undefined && data.chat_id !== chatId) ||
        generating(data) ||
        data.messages.some(
          (message) =>
            !message ||
            typeof message !== "object" ||
            (message.chat_id !== undefined && message.chat_id !== chatId) ||
            generating(message),
        )
      )
        return failure("retained", "messages", "unverified_messages");
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
        return failure("retained", "messages", "pagination_incomplete");
      if (
        typeof next !== "string" ||
        next.length > 512 ||
        tokens.has(next) ||
        index + 1 >= maxPages
      )
        return failure("retained", "messages", "pagination_incomplete");
      tokens.add(next);
      pageToken = next;
    }
    return failure("retained", "messages", "pagination_incomplete");
  } catch {
    return failure("unknown", stage, "transport_unknown");
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
    return failure("retained", "scope", "invalid_scope");
  if (typeof authorizeDeletion !== "function")
    return failure("retained", "authorization", "authorization_required");
  let stage = "inspection";
  try {
    const first = await inspect(page, options);
    if (first.status !== "matched") return first;
    let authority;
    stage = "authorization";
    try {
      authority = await authorizeDeletion({
        provider: "kimi",
        platform_account_id: expectedUserId,
        external_conversation_id: chatId,
      });
    } catch {
      return failure("retained", "authorization", "authorization_required");
    }
    if (
      authority?.authorized !== true ||
      authority.platform_account_id !== expectedUserId ||
      authority.external_conversation_id !== chatId ||
      typeof authority.retained_message_inventory_sha256 !== "string" ||
      !INVENTORY_SHA256.test(authority.retained_message_inventory_sha256)
    )
      return failure("retained", "authorization", "authorization_required");
    // Re-check after the awaited ledger authorization: the account or
    // conversation may have changed while the caller consulted durable state.
    stage = "inspection";
    const second = await recoverKimiConversation(page, options);
    if (second.status !== "recovered") return second;
    // Ownership of the chat does not imply ownership/durability of every turn:
    // a user may have appended messages since the original capture. Require
    // complete bounded pagination and an exact retained-message inventory.
    // This cannot atomically exclude edits between this read and provider delete.
    stage = "inventory";
    const inventory = retainedMessageInventory(second.message_pages);
    if (!inventory || inventory !== authority.retained_message_inventory_sha256)
      return failure("retained", "inventory", "message_inventory_mismatch");
    // Production authority carries the claim/ticket deadline. Recheck after
    // the final read, immediately before the only destructive request.
    const deleteNotAfterMs =
      authority.delete_not_after === undefined
        ? undefined
        : Date.parse(authority.delete_not_after);
    if (
      authority.delete_not_after !== undefined &&
      (typeof authority.delete_not_after !== "string" ||
        !Number.isFinite(deleteNotAfterMs) ||
        deleteNotAfterMs - Date.now() < timeoutMs + DELETE_CLOSE_MARGIN_MS)
    )
      return {
        ...failure("retained", "authorization", "authorization_expired"),
        reason: "authorization_required",
      };
    stage = "delete";
    const response = await connect(page, {
      ...options,
      path: DELETE_CHAT,
      body: { chat_id: chatId },
      deleteNotAfterMs,
    });
    if (response.kind === "authorization_expired")
      return {
        ...failure("retained", "authorization", "authorization_expired"),
        reason: "authorization_required",
      };
    if (response.kind !== "ok")
      return failure(
        response.kind === "reauth_required" ? "reauth_required" : "unknown",
        "delete",
        response.kind,
      );
    if (response.data?.chat_id !== chatId)
      return failure("unknown", "delete", "unverified_delete_response");
    return { status: "deleted", external_conversation_id: chatId };
  } catch {
    // The delete may have reached the provider. Never retry automatically.
    return failure("unknown", stage, "transport_unknown");
  }
}
