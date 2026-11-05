import i18n from "./index";

const translations = {
  "zh-CN": {
    title: "对话中的测量结果",
    loading: "正在读取测量结果",
    unavailable: "暂时无法读取这次测量，请重试。",
    updateFailed: "最新状态暂时无法读取，以下保留已有结果。",
    retry: "重新读取",
    discuss: "讨论这次测量",
    preserved: "你的草稿已保留，可编辑后继续讨论。",
  },
  en: {
    title: "Measurement results in this conversation",
    loading: "Loading measurement results",
    unavailable: "This measurement could not be loaded. Please try again.",
    updateFailed:
      "The latest status could not be loaded. Saved results remain below.",
    retry: "Try again",
    discuss: "Discuss this measurement",
    preserved: "Your draft is preserved. Edit it to continue the discussion.",
  },
};
for (const [language, translation] of Object.entries(translations)) {
  i18n.addResourceBundle(
    language,
    "agentMeasurementResults",
    translation,
    true,
    true,
  );
}
