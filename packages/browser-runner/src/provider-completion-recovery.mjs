import { readDeepSeekFirstTurnHistory } from "./deepseek-web-history.mjs";
import {
  deepSeekRecoveryBinding,
  recoveredDeepSeekCompletion,
} from "./deepseek-conversation-evidence.mjs";

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
