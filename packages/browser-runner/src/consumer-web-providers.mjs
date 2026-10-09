// Public consumer-web entry points, not API inference endpoints. Entry
// navigation and verified account completion are separate capabilities.
// Session isolation, proxying and remote interaction stay in the shared runner.
export const consumerWebProviders = Object.freeze({
  kimi: Object.freeze({
    origin: "https://www.kimi.com",
    entry: "https://www.kimi.com/",
    loginEntryAvailable: true,
    loginSupported: true,
  }),
  doubao: Object.freeze({
    origin: "https://www.doubao.com",
    entry: "https://www.doubao.com/chat/",
    loginEntryAvailable: true,
    loginSupported: true,
  }),
  deepseek: Object.freeze({
    origin: "https://chat.deepseek.com",
    entry: "https://chat.deepseek.com/",
    loginEntryAvailable: true,
    loginSupported: true,
  }),
  glm: Object.freeze({
    origin: "https://chatglm.cn",
    entry: "https://chatglm.cn/",
    loginEntryAvailable: true,
    loginSupported: true,
  }),
});
