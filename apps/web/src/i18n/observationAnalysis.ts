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
      "解析内容与原始记录未能核对一致，不能作为已确认答案。你可以查看原始记录后，再决定是否重新解析。",
    failed: "这次解析未完成。已存原始记录保留，你可以稍后重新解析。",
    failureConfiguration: "解析模型尚未配置好。请检查解析模型设置后重新解析。",
    failureAccess:
      "无法访问解析模型。请检查模型的访问凭据和使用权限后重新解析。",
    failureBudget: "解析模型的可用额度不足。请检查额度后重新解析。",
    failureUnavailable:
      "解析服务暂时无法连接或不可用。已存原始记录保留，你可以稍后重新解析。",
    failureRateLimit:
      "解析服务当前请求过多。已存原始记录保留，请稍后再重新解析。",
    failureTimeout: "等待解析结果超时。已存原始记录保留，你可以稍后重新解析。",
    failureInterrupted:
      "这次解析未完成，处理已中断或取消。已存原始记录保留，你可以重新解析。",
    failureResponse:
      "模型未返回可用的完整解析结果。已存原始记录保留，你可以重新解析或选择其他解析模型。",
    failureRequest:
      "解析服务未接受这次请求。请检查所选模型是否可用后重新解析。",
    failureSource: "这次解析无法读取已存原始记录。请刷新测量记录后再试。",
    failureGrounding:
      "暂时无法核对解析内容与原始记录，结果尚不能作为已确认答案。请稍后重新解析。",
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
      "The analysis could not be verified against the source and is not a confirmed answer. Review the source record before deciding whether to reanalyze.",
    failed:
      "This analysis did not finish. The saved source record is retained; you can reanalyze later.",
    failureConfiguration:
      "The analysis model is not configured. Check its settings before reanalyzing.",
    failureAccess:
      "The analysis model could not be accessed. Check its credentials and permissions before reanalyzing.",
    failureBudget:
      "The analysis model has insufficient available allowance. Check the allowance before reanalyzing.",
    failureUnavailable:
      "The analysis service could not be reached or is temporarily unavailable. The saved source record is retained; you can reanalyze later.",
    failureRateLimit:
      "The analysis service is receiving too many requests. The saved source record is retained; try reanalyzing later.",
    failureTimeout:
      "Waiting for the analysis result timed out. The saved source record is retained; you can reanalyze later.",
    failureInterrupted:
      "This analysis did not finish because it was interrupted or cancelled. The saved source record is retained; you can reanalyze.",
    failureResponse:
      "The model did not return a usable, complete analysis. The saved source record is retained; reanalyze or choose another analysis model.",
    failureRequest:
      "The analysis service did not accept this request. Check that the selected model is available before reanalyzing.",
    failureSource:
      "The saved source record could not be read for this analysis. Refresh the measurement record before trying again.",
    failureGrounding:
      "The analysis could not be checked against the source record yet and is not a confirmed answer. You can reanalyze later.",
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

/** Map only fixed service codes; never render provider text or unknown codes. */
export function analysisFailureMessageKey(code: string): string {
  const messages: Record<string, string> = {
    analysis_source_unavailable: "failureSource",
    model_request_invalid: "failureRequest",
    model_unconfigured: "failureConfiguration",
    model_settings_unavailable: "failureUnavailable",
    model_access_denied: "failureAccess",
    model_http_unauthorized: "failureAccess",
    model_http_forbidden: "failureAccess",
    model_budget_exceeded: "failureBudget",
    model_timeout: "failureTimeout",
    model_http_timeout: "failureTimeout",
    analysis_timeout: "failureTimeout",
    model_cancelled: "failureInterrupted",
    analysis_interrupted: "failureInterrupted",
    interrupted: "failureInterrupted",
    model_http_rate_limited: "failureRateLimit",
    model_http_server_error: "failureUnavailable",
    model_transport_failed: "failureUnavailable",
    model_http_rejected: "failureRequest",
    model_response_too_large: "failureResponse",
    model_response_invalid: "failureResponse",
    analysis_result_invalid: "failureResponse",
    model_output_too_large: "failureResponse",
    model_output_incomplete: "failureResponse",
    model_output_tool_calls: "failureResponse",
    model_output_invalid: "failureResponse",
    grounding_unavailable: "failureGrounding",
  };
  return Object.hasOwn(messages, code) ? messages[code] : "failed";
}

export function analysisUnverifiedMessageKey(reason: string): string {
  return reason.startsWith("model_output_")
    ? analysisFailureMessageKey(reason) === "failureResponse"
      ? "failureResponse"
      : "unverified"
    : "unverified";
}

for (const [language, translation] of Object.entries(translations)) {
  i18n.addResourceBundle(
    language,
    "observationAnalysis",
    translation,
    true,
    true,
  );
}
