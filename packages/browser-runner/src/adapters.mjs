// Adapter facade.
//
// Every provider lives in ./providers/<provider>.mjs beside the shared
// ./providers/contract.mjs, and the frozen provider map lives in
// ./providers/registry.mjs. This module only re-exports, so existing callers
// and tests keep working without importing provider internals.
export {
  glmIdentity,
  deepseekIdentity,
  doubaoIdentity,
  qianwenIdentity,
  probeGlmAccount,
  probeDeepseekAccount,
  probeDoubaoAccount,
  probeQianwenAccount,
} from "./consumer-web-identity.mjs";
export { CONNECTOR_VERSION, probeOwnAccount } from "./providers/contract.mjs";
export { adapters } from "./providers/registry.mjs";
export { baiduIdentity, publishBaidu } from "./providers/baidu-creator.mjs";
export { measureDeepseek } from "./providers/deepseek.mjs";
export {
  extractKimiCandidateObservation,
  kimiIdentity,
  measureKimi,
  observeKimiSearchAttempt,
  probeKimiAccount,
} from "./providers/kimi.mjs";
export { xiaohongshuIdentity } from "./providers/xiaohongshu.mjs";
export {
  publishZhihu,
  readbackZhihu,
  zhihuIdentity,
} from "./providers/zhihu.mjs";
