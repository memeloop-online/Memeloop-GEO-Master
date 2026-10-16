import i18n from "./index";

const translations = {
  "zh-CN": {
    open: "排名变化",
    close: "收起排名变化",
    revisionMissing: "找不到链接指定的解析版本，请返回测量记录查看。",
  },
  en: {
    open: "Rank history",
    close: "Hide rank history",
    revisionMissing:
      "The linked analysis revision was not found. Return to the measurement record to review available results.",
  },
};

for (const [language, translation] of Object.entries(translations)) {
  i18n.addResourceBundle(
    language,
    "questionSetRecommendationTrend",
    translation,
    true,
    true,
  );
}
