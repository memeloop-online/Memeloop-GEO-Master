// The single source of provider adapters. Key order is observable through
// capabilities(), so keep the historical order: zhihu, baidu_creator,
// xiaohongshu, kimi, glm, deepseek, doubao, qianwen, yuanbao.
import { defineRegistry } from "./contract.mjs";
import { baiduCreatorAdapter } from "./baidu-creator.mjs";
import { deepseekAdapter } from "./deepseek.mjs";
import { doubaoAdapter } from "./doubao.mjs";
import { glmAdapter } from "./glm.mjs";
import { kimiAdapter } from "./kimi.mjs";
import { qianwenAdapter } from "./qianwen.mjs";
import { xiaohongshuAdapter } from "./xiaohongshu.mjs";
import { yuanbaoAdapter } from "./yuanbao.mjs";
import { zhihuAdapter } from "./zhihu.mjs";

export const adapters = defineRegistry({
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
