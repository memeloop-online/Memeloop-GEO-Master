import i18n from "./index";

const translations = {
  "zh-CN": {
    title: "已存回答解析",
    boundary:
      "仅解析已保存的原始记录，不向被测平台重新提问。原测量状态保持不变，解析结果单独保存。",
    parse: "重新解析",
    submitting: "正在提交解析",
    retrySubmit: "重试提交解析",
    submitFailed: "解析提交尚未确认。请刷新记录核对，或重试同一次提交。",
    loading: "正在读取解析记录",
    loadFailed: "暂时无法读取解析记录，请刷新后重试。",
    refresh: "刷新解析记录",
    noSource: "没有可解析的已存原始记录。",
    noRevisions: "尚未解析已存记录。",
    queued: "等待解析",
    running: "正在解析",
    grounded: "已与原始记录核对",
    unverified:
      "解析未通过原始记录核对，不能作为已确认答案。可检查解析模型后重新解析。",
    failed: "这次解析未完成。请检查模型配置后重新解析。",
    incomplete: "尚无可确认的解析结果，请刷新记录。",
    model: "实际解析模型：{{value}}",
    modelMissing: "尚未记录实际解析模型",
    requested: "提交时间：{{value}}",
    analyzed: "解析时间：{{value}}",
    observed: "原始记录时间：{{value}}",
    source: "来源：本次测量保存的原始记录",
    settings: "配置解析模型",
    answer: "解析出的答案",
    citations: "解析出的引用",
    noCitations: "没有可显示的引用链接。",
    evidence: "查看原文定位依据",
    more: "更早的解析记录",
    readOnly: "你可以查看解析记录，但没有重新解析权限。",
  },
  en: {
    title: "Saved response analysis",
    boundary:
      "Analyze only saved source records without asking the measured platform again. The original measurement stays unchanged; analyses are saved separately.",
    parse: "Reanalyze",
    submitting: "Submitting analysis",
    retrySubmit: "Retry analysis submission",
    submitFailed:
      "Analysis submission is not confirmed. Refresh to check, or retry the same submission.",
    loading: "Loading analyses",
    loadFailed: "Analyses are unavailable. Refresh to retry.",
    refresh: "Refresh analyses",
    noSource: "No saved source record is available for analysis.",
    noRevisions: "No saved-source analyses yet.",
    queued: "Waiting for analysis",
    running: "Analyzing",
    grounded: "Checked against the source record",
    unverified:
      "The analysis could not be verified against the source and is not a confirmed answer. Check the analysis model and try again.",
    failed: "This analysis did not finish. Check model settings and try again.",
    incomplete: "No confirmed analysis result is available. Refresh to check.",
    model: "Actual analysis model: {{value}}",
    modelMissing: "Actual analysis model not yet recorded",
    requested: "Requested: {{value}}",
    analyzed: "Analyzed: {{value}}",
    observed: "Source recorded: {{value}}",
    source: "Source: original record saved for this measurement",
    settings: "Configure analysis model",
    answer: "Extracted answer",
    citations: "Extracted citations",
    noCitations: "No citation links to display.",
    evidence: "View source grounding",
    more: "Earlier analyses",
    readOnly: "You can view analyses but do not have permission to reanalyze.",
  },
};

for (const [language, translation] of Object.entries(translations)) {
  i18n.addResourceBundle(
    language,
    "observationAnalysis",
    translation,
    true,
    true,
  );
}
