import {
  recoverKimiConversation,
  deleteKimiConversation,
} from "./provider-conversation-cleanup.mjs";
import {
  recoverDeepSeekConversation,
  deleteDeepSeekConversation,
} from "./deepseek-conversation-cleanup.mjs";

const providers = Object.freeze({
  kimi: Object.freeze({
    recover: recoverKimiConversation,
    delete: deleteKimiConversation,
  }),
  deepseek: Object.freeze({
    recover: recoverDeepSeekConversation,
    delete: deleteDeepSeekConversation,
  }),
});

// Selection comes from the authenticated account, never a response body.
export function providerCleanupAdapter(provider) {
  return typeof provider === "string" && Object.hasOwn(providers, provider)
    ? providers[provider]
    : null;
}
