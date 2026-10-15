import { readDeepSeekFirstTurnHistory } from "./deepseek-web-history.mjs";
import { validDeepSeekSessionId } from "./deepseek-web-transport.mjs";

const messageId = (value) =>
  typeof value === "string" &&
  /^(?:0|[1-9]\d{0,15})$/u.test(value) &&
  Number.isSafeInteger(Number(value)) &&
  String(Number(value)) === value;

const providers = Object.freeze({
  deepseek: Object.freeze({
    validScope: (input) =>
      validDeepSeekSessionId(input.external_conversation_id) &&
      messageId(input.request_message_id) &&
      messageId(input.response_message_id) &&
      input.request_message_id !== input.response_message_id,
    read: (page, input, options) =>
      readDeepSeekFirstTurnHistory(page, {
        ...options,
        // The ledger separately validates terminal error ownership before
        // using an empty inventory; this never certifies answer completion.
        allowEmptyHistory: true,
        conversationId: input.external_conversation_id,
        requestMessageId: input.request_message_id,
        responseMessageId: input.response_message_id,
      }),
  }),
});

// Account-selected, read-only providers. No measurement or deletion fallback.
export function providerCompletionRecoveryAdapter(provider) {
  return typeof provider === "string" && Object.hasOwn(providers, provider)
    ? providers[provider]
    : null;
}
