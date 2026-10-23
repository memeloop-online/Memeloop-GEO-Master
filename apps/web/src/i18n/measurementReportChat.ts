import i18n from "./index";

const translations = {
  "zh-CN": {
    selectedQuestion:
      "仅分析所选问题（样本编号：{{questionTargetId}}）及其相同问题的测量结果，不扩展到其他问题。",
    request:
      "请分析这份独立测量报告（编号：{{reportId}}）中当前项目关注产品或品牌的推荐表现。按相同问题和测量条件比较：它与排名更靠前的产品各有什么推荐理由？哪些原文和引用支持这些结论？请区分实际观察与推测，给出有依据的改进建议及需要补充的信息。仅分析已有结果，不发起测量或发布。先概括产品表现，再比较竞争产品，最后按优先级给出改进建议。使用简短段落或要点，不默认展开大表格，不复述内部编号或字段名。证据不足或读取失败时，简要说明哪些结论暂时无法判断，以及当前可采取的下一步；不要编造发现或要求用户复制内部数据结构。",
  },
  en: {
    selectedQuestion:
      "Analyze only the selected question (sample ID: {{questionTargetId}}) and measurements of that same question; do not include other questions.",
    request:
      "Analyze how the current project's tracked product or brand performed in this independent measurement report (ID: {{reportId}}). Compare matching questions and measurement conditions: why was it recommended, and why were higher-ranked products recommended? Which excerpts and citations support those findings? Separate observations from hypotheses, suggest evidence-based improvements, and identify missing information. Analyze existing results only; do not start measurements or publish content. Start with product performance, compare competitors, then prioritize improvements. Use short paragraphs or bullets rather than large tables by default; do not repeat internal IDs or field names. If evidence is insufficient or retrieval fails, briefly state which conclusions remain unavailable and the next available action. Do not invent findings or ask the user to copy internal data structures.",
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
