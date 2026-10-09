import i18n from "./index";

const translations = {
  "zh-CN": {
    projectAi: {
      tab: "模型与 AI",
      accounts: "账号与资源",
      tabs: "项目设置分区",
      title: "模型与 AI",
      description: "分别选择工作台与内容生成模型，以及测量结果的解析模型。",
      boundary:
        "这里的模型用于推理或解析已采集的回答，不会替换被测平台、网页账号或测量模型。",
      inference: "工作台与内容生成",
      inferenceHelp: "用于 AI 对话、工具选择和内容生成。",
      observation: "测量结果解析",
      observationHelp:
        "从已采集的原始回答中提取答案与引用，不重新向被测平台提问。",
      mode: "模型来源",
      inherited: "使用默认配置",
      effectiveModel: "当前模型：{{model}}",
      defaultConfigured: "默认模型已配置。",
      discover: "获取模型列表",
      discovering: "正在获取模型列表",
      discoveredModels: "可用模型",
      selectModel: "选择模型，或在模型名称中手动输入",
      discoveryFailed: "无法获取模型列表。你仍可以手动输入模型名称。",
      noModels: "未返回可用模型。请手动输入模型名称。",
      saveFirst: "请先保存修改，再测试或获取模型列表。",
      readbackFailed: "配置已保存，但暂时无法重新读取。请重新读取后继续。",
      unavailable: "当前尚无可用的默认模型。",
      configured: "使用项目专属配置",
      endpoint: "API 地址",
      model: "模型名称",
      apiKey: "API 密钥",
      keySaved: "已保存密钥；留空保留原密钥。",
      keyMissing: "尚未保存密钥。",
      keyHelp: "密钥仅用于服务端调用，保存后不再显示。",
      preferBrowser: "优先使用已登录网页账号解析",
      browserHelp: "网页账号解析不可用时，使用下方配置的 API 解析模型。",
      save: "保存配置",
      saving: "正在保存",
      saved: "配置已保存。",
      dirty: "有尚未保存的修改。",
      reset: "放弃修改",
      test: "测试已保存配置",
      testing: "正在测试",
      testHelp:
        "先保存修改再测试。测试会发起一次模型请求，可能产生费用；不会发送测量问题。",
      testPassed: "模型连接成功。",
      testFailed: "模型测试失败，请检查配置后重试。",
      loadFailed: "暂时无法读取 AI 配置。",
      saveFailed: "配置未保存，请检查配置后重试。",
      conflict: "配置已被更新。请重新读取最新配置后再修改。",
      reload: "重新读取配置",
      loading: "正在读取 AI 配置",
      readOnly: "你可以查看配置，但没有修改或测试权限。",
    },
  },
  en: {
    projectAi: {
      tab: "Models & AI",
      accounts: "Accounts & resources",
      tabs: "Project settings sections",
      title: "Models & AI",
      description:
        "Choose models for the workbench and content, and separately for parsing measurement results.",
      boundary:
        "These models handle inference or captured answers. They do not replace the measured platform, browser account, or measurement model.",
      inference: "Workbench & content",
      inferenceHelp:
        "Used for AI conversations, tool selection, and content generation.",
      observation: "Measurement result parsing",
      observationHelp:
        "Extract answers and citations from captured responses without asking the measured platform again.",
      mode: "Model source",
      inherited: "Use default configuration",
      effectiveModel: "Current model: {{model}}",
      defaultConfigured: "A default model is configured.",
      discover: "Get model list",
      discovering: "Loading model list",
      discoveredModels: "Available models",
      selectModel: "Select a model or enter its name manually",
      discoveryFailed:
        "Unable to load models. You can still enter a model name manually.",
      noModels: "No models were returned. Enter a model name manually.",
      saveFirst: "Save your changes before testing or loading models.",
      readbackFailed:
        "Configuration was saved but could not be reloaded. Reload it before continuing.",
      unavailable: "No default model is currently available.",
      configured: "Use project configuration",
      endpoint: "API base URL",
      model: "Model name",
      apiKey: "API key",
      keySaved: "A key is saved. Leave blank to keep it.",
      keyMissing: "No key has been saved.",
      keyHelp:
        "The key is used only on the server and is never shown after saving.",
      preferBrowser: "Prefer a signed-in browser account for parsing",
      browserHelp:
        "If browser parsing is unavailable, use the API parsing model configured below.",
      save: "Save configuration",
      saving: "Saving",
      saved: "Configuration saved.",
      dirty: "You have unsaved changes.",
      reset: "Discard changes",
      test: "Test saved configuration",
      testing: "Testing",
      testHelp:
        "Save changes before testing. A test sends one model request and may incur a charge; it does not send a measurement question.",
      testPassed: "Model connection succeeded.",
      testFailed: "Model test failed. Check the configuration and retry.",
      loadFailed: "AI configuration is currently unavailable.",
      saveFailed: "Configuration was not saved. Check it and retry.",
      conflict: "Configuration has changed. Reload it before editing again.",
      reload: "Reload configuration",
      loading: "Loading AI configuration",
      readOnly: "You can view this configuration but cannot change or test it.",
    },
  },
};

for (const [language, translation] of Object.entries(translations)) {
  i18n.addResourceBundle(language, "translation", translation, true, true);
}
