import { createHash } from "node:crypto";
import { probeDeepseekAccount } from "./consumer-web-identity.mjs";
import { cleanupDiagnostic } from "./provider-conversation-cleanup.mjs";
import { projectDeepSeekCleanupHistory } from "./deepseek-web-history.mjs";
import {
  DEEPSEEK_ORIGIN,
  validDeepSeekSessionId,
  withDeepSeekDocument,
} from "./deepseek-web-transport.mjs";

const SHA256 = /^[0-9a-f]{64}$/u;
const failure = (status, stage, code) => ({
  status,
  reason: code,
  diagnostic: cleanupDiagnostic({ stage, code }),
});
const unknown = (stage) => failure("unknown", stage, "transport_unknown");
const optionsValid = ({ trustedOrigin, expectedUserId, chatId, timeoutMs }) =>
  trustedOrigin === DEEPSEEK_ORIGIN &&
  validDeepSeekSessionId(expectedUserId) &&
  validDeepSeekSessionId(chatId) &&
  Number.isInteger(timeoutMs) &&
  timeoutMs >= 1 &&
  timeoutMs <= 30_000;

// Same canonical sorted ID/role tuples as the persisted ownership inventory.
function inventoryDigest(inventory) {
  const tuples = inventory.messages.map(({ id, role }) => [id, role]);
  tuples.sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0));
  return createHash("sha256").update(JSON.stringify(tuples)).digest("hex");
}

async function identity(page, scope, expectedUserId) {
  if (!(await scope.check())) return unknown("identity");
  const account = await probeDeepseekAccount(page);
  if (!(await scope.check())) return unknown("identity");
  if (!account)
    return failure("reauth_required", "identity", "reauth_required");
  if (account.platform_account_id !== expectedUserId)
    return failure("retained", "identity", "account_mismatch");
  return null;
}

async function history(scope, chatId) {
  return projectDeepSeekCleanupHistory(
    await scope.request("history", chatId),
    chatId,
  );
}

/**
 * Passive recovery only. verifyAbsence is reserved for the domain's existing
 * prior-attempt reconciliation policy; an initial missing chat is not deleted.
 * No provider content, credentials or account profile is returned.
 */
export async function recoverDeepSeekConversation(
  page,
  {
    trustedOrigin = DEEPSEEK_ORIGIN,
    expectedUserId,
    chatId,
    timeoutMs = 12_000,
    signal,
    verifyAbsence = false,
  } = {},
) {
  if (!optionsValid({ trustedOrigin, expectedUserId, chatId, timeoutMs }))
    return failure("retained", "scope", "invalid_scope");
  return (
    (await withDeepSeekDocument(page, { timeoutMs, signal }, async (scope) => {
      const mismatch = await identity(page, scope, expectedUserId);
      if (mismatch) return mismatch;
      const result = await history(scope, chatId);
      if (!(await scope.check())) return unknown("inspection");
      if (verifyAbsence === true && result.status === "absent")
        return { status: "absent", external_conversation_id: chatId };
      if (result.status !== "verified")
        return failure("unknown", "inspection", "unverified_messages");
      return {
        status: "recovered",
        external_conversation_id: chatId,
        inventory: result.inventory,
      };
    })) ?? unknown("inspection")
  );
}

/**
 * One exact POST after independent ledger authorization, then strict fresh
 * absence readback. Authorization must establish durable retained originals,
 * system ownership, no live task references and the account lock/ticket.
 * Neither candidates nor observed history establish that authority.
 * Unknown responses never trigger a retry. The provider has no conditional
 * delete: a read cannot atomically exclude later external user edits.
 */
export async function deleteDeepSeekConversation(
  page,
  {
    trustedOrigin = DEEPSEEK_ORIGIN,
    expectedUserId,
    chatId,
    timeoutMs = 12_000,
    signal,
    authorizeDeletion,
  } = {},
) {
  if (!optionsValid({ trustedOrigin, expectedUserId, chatId, timeoutMs }))
    return failure("retained", "scope", "invalid_scope");
  if (typeof authorizeDeletion !== "function")
    return failure("retained", "authorization", "authorization_required");
  let stage = "inspection";
  return (
    (await withDeepSeekDocument(page, { timeoutMs, signal }, async (scope) => {
      let mismatch = await identity(page, scope, expectedUserId);
      if (mismatch) return mismatch;
      const first = await history(scope, chatId);
      if (first.status !== "verified")
        return failure("retained", "inspection", "unverified_messages");
      if (!(await scope.check())) return unknown(stage);
      stage = "authorization";
      let authority;
      try {
        authority = await authorizeDeletion({
          provider: "deepseek",
          platform_account_id: expectedUserId,
          external_conversation_id: chatId,
        });
      } catch {
        return failure("retained", stage, "authorization_required");
      }
      if (
        authority?.authorized !== true ||
        authority.platform_account_id !== expectedUserId ||
        authority.external_conversation_id !== chatId ||
        typeof authority.retained_message_inventory_sha256 !== "string" ||
        !SHA256.test(authority.retained_message_inventory_sha256)
      )
        return failure("retained", stage, "authorization_required");
      // Copy primitives before any further await; a mutable grant cannot
      // replace its retained digest or deadline during fresh provider reads.
      const digest = authority.retained_message_inventory_sha256;
      const expires = authority.delete_not_after;
      const deleteNotAfterMs =
        typeof expires === "string" ? Date.parse(expires) : NaN;
      const expired = () =>
        !Number.isFinite(deleteNotAfterMs) ||
        deleteNotAfterMs - Date.now() < timeoutMs + 2_000;
      const expiredResult = () => ({
        ...failure("retained", "authorization", "authorization_expired"),
        reason: "authorization_required",
      });
      if (expired()) return expiredResult();
      if (inventoryDigest(first.inventory) !== digest)
        return failure("retained", "inventory", "message_inventory_mismatch");
      stage = "identity";
      mismatch = await identity(page, scope, expectedUserId);
      if (mismatch) return mismatch;
      stage = "inventory";
      const second = await history(scope, chatId);
      if (
        second.status !== "verified" ||
        inventoryDigest(second.inventory) !== digest
      )
        return failure("retained", stage, "message_inventory_mismatch");
      if (!(await scope.check())) return unknown(stage);
      if (expired()) return expiredResult();
      stage = "delete";
      const envelope = await scope.request("delete", chatId, {
        deleteNotAfterMs,
      });
      if (envelope?.kind === "authorization_expired") return expiredResult();
      let data;
      try {
        data = envelope?.status === 200 ? JSON.parse(envelope.body) : null;
      } catch {
        return failure("unknown", stage, "unverified_delete_response");
      }
      // Public first-party client contract: POST exact chat_session_ids array;
      // HTTP 200 + outer code 0 + biz_code 0. No nonzero code means absence.
      if (data?.code !== 0 || data?.data?.biz_code !== 0)
        return failure("unknown", stage, "unverified_delete_response");
      mismatch = await identity(page, scope, expectedUserId);
      if (mismatch) return unknown(stage);
      const readback = await history(scope, chatId);
      if (readback.status !== "absent" || !(await scope.check()))
        return failure("unknown", stage, "unverified_delete_response");
      return { status: "deleted", external_conversation_id: chatId };
    })) ?? unknown(stage)
  );
}
