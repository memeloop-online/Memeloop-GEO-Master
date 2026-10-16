import i18n from "./index";

const translations = {
  "zh-CN": {
    open: "排名变化",
    close: "收起排名变化",
    loading: "正在读取历次测量与推荐结果",
    failed: "排名变化暂时无法读取",
    retryDetail: "请重试读取测量记录。",
    forbidden: "当前无法访问这些测量记录。请检查登录状态或项目权限。",
    scope: "比较该问题集版本的全部测量记录，按相同问题和测量条件分别展示。",
    live: "这是当前已保存结果的只读比较，后续解析可能更新展示内容。",
    noRecords: "该问题集版本还没有测量记录。",
    refresh: "刷新排名变化",
    revisionMissing: "找不到链接指定的解析版本，请返回测量记录查看。",
  },
  en: {
    open: "Rank history",
    close: "Hide rank history",
    loading: "Loading measurements and recommendation results",
    failed: "Rank history could not be loaded",
    retryDetail: "Try loading the measurement records again.",
    forbidden:
      "These measurements are unavailable. Check your sign-in status or project access.",
    scope:
      "Compare all measurements of this question set version, grouped by matching questions and measurement conditions.",
    live: "This is a read-only comparison of currently saved results. Further analysis may update this view.",
    noRecords: "This question set version has no measurements yet.",
    refresh: "Refresh rank history",
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
