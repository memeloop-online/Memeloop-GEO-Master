import i18n from "./index";

const translations = {
  "zh-CN": {
    request:
      "请分析这份独立测量报告（编号：{{reportId}}）中当前项目关注产品或品牌的推荐表现。按相同问题和测量条件比较：它与排名更靠前的产品各有什么推荐理由？哪些原文和引用支持这些结论？请区分实际观察与推测，给出有依据的改进建议及需要补充的信息。仅分析已有结果，不发起测量或发布。",
  },
  en: {
    request:
      "Analyze how the current project's tracked product or brand performed in this independent measurement report (ID: {{reportId}}). Compare matching questions and measurement conditions: why was it recommended, and why were higher-ranked products recommended? Which excerpts and citations support those findings? Separate observations from hypotheses, suggest evidence-based improvements, and identify missing information. Analyze existing results only; do not start measurements or publish content.",
  },
};

for (const [language, translation] of Object.entries(translations)) {
  i18n.addResourceBundle(
    language,
    "measurementReportChat",
    translation,
    true,
    true,
  );
}
