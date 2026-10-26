import { readDeepSeekFirstTurnHistory } from "./deepseek-web-history.mjs";
import {
  deepSeekRecoveryBinding,
  recoveredDeepSeekCompletion,
} from "./deepseek-conversation-evidence.mjs";
import { recoverKimiConversation } from "./provider-conversation-cleanup.mjs";
import {
  kimiRecoveryBinding,
  recoveredKimiCompletion,
} from "./kimi-conversation-evidence.mjs";

function savedDeepSeekSource(input) {
  try {
    const invalidIds = new WeakSet();
    const source = JSON.parse(
      input.source_json,
      function (key, value, context) {
        if (
          [
            "request_message_id",
            "response_message_id",
            "message_id",
            "parent_id",
            "current_message_id",
          ].includes(key) &&
          typeof value === "number" &&
          (!/^(?:0|[1-9]\d{0,15})$/u.test(context?.source ?? "") ||
            String(value) !== context.source)
        )
          invalidIds.add(this);
        return value;
      },
    );
    return Array.isArray(source?.messages) &&
      !source.messages.some(
        (message) => message?.event === "ready" && invalidIds.has(message.data),
      )
      ? source
      : null;
  } catch {
    return null;
  }
}

const providers = Object.freeze({
  kimi: Object.freeze({
    async read(page, input, options = {}) {
      let source;
      try {
        source = JSON.parse(input.source_json);
      } catch {
        options.onFailureReason?.("source_ineligible");
        return null;
      }
      const binding = kimiRecoveryBinding(
        source,
        input.external_conversation_id,
      );
      if (!binding) {
        options.onFailureReason?.("source_ineligible");
        return null;
      }
      const history = await recoverKimiConversation(page, {
        expectedUserId: input.expected_identity.platform_account_id,
        chatId: input.external_conversation_id,
        timeoutMs: options.timeoutMs,
      });
      const completion = recoveredKimiCompletion(
        binding,
        history,
        input.external_conversation_id,
      );
      if (!completion) {
        options.onFailureReason?.(
          history.reason === "transport_unknown"
            ? "transport_unavailable"
            : "inventory_unverified",
        );
        return null;
      }
      const observed_at = new Date().toISOString();
      return {
        completion,
        observed_at,
        history_json: JSON.stringify({
          source: "browser_observed_history",
          observed_at,
          chat: history.chat,
          message_pages: history.message_pages,
        }),
      };
    },
  }),
  deepseek: Object.freeze({
    async read(page, input, options = {}) {
      const source = savedDeepSeekSource(input);
      const binding = deepSeekRecoveryBinding(
        source,
        input.external_conversation_id,
      );
      if (!binding) {
        options.onFailureReason?.("source_ineligible");
        return null;
      }
      const result = await readDeepSeekFirstTurnHistory(page, {
        ...options,
        allowEmptyHistory: true,
        conversationId: input.external_conversation_id,
        ...binding,
      });
      if (!result) return null;
      const completion = recoveredDeepSeekCompletion(
        source,
        result.history,
        input.external_conversation_id,
      );
      return completion
        ? {
            completion,
            observed_at: result.history.observed_at,
            history_json: JSON.stringify(result.history),
          }
        : null;
    },
  }),
});

// Account-selected, read-only providers. No measurement or deletion fallback.
export function providerCompletionRecoveryAdapter(provider) {
  return typeof provider === "string" && Object.hasOwn(providers, provider)
    ? providers[provider]
    : null;
}
