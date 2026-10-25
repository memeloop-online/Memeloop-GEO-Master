// Adapter facade and registry.
//
// Every provider lives in ./providers/<provider>.mjs beside the shared
// ./providers/contract.mjs. This module keeps the historical export surface
// and the frozen `adapters` map so existing callers and tests stay unchanged.
import { baiduCreatorAdapter } from "./providers/baidu-creator.mjs";
import { deepseekAdapter } from "./providers/deepseek.mjs";
import { doubaoAdapter } from "./providers/doubao.mjs";
import { glmAdapter } from "./providers/glm.mjs";
import { kimiAdapter } from "./providers/kimi.mjs";
import { qianwenAdapter } from "./providers/qianwen.mjs";
import { xiaohongshuAdapter } from "./providers/xiaohongshu.mjs";
import { yuanbaoAdapter } from "./providers/yuanbao.mjs";
import { zhihuAdapter } from "./providers/zhihu.mjs";

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

export const adapters = Object.freeze({
  zhihu: zhihuAdapter,
  baidu_creator: baiduCreatorAdapter,
  xiaohongshu: xiaohongshuAdapter,
  kimi: kimiAdapter,
  glm: glmAdapter,
  deepseek: deepseekAdapter,
  doubao: doubaoAdapter,
  qianwen: qianwenAdapter,
  yuanbao: yuanbaoAdapter,
});
