import i18n from "../i18n";

const zh = {
  title: "从话题生成候选问题",
  description:
    "输入一个词或话题，让 AI 提出可供编辑和测量的候选问题。预测不是实际用户提问或搜索量。",
  topic: "话题",
  placeholder: "例如：家庭储能",
  market: "市场（可选）",
  language: "问题语言（可选）",
  marketHint: "留空时使用当前项目设置；尚未设置则使用 CN。",
  languageHint: "留空时使用当前项目设置；尚未设置则使用 zh-CN。",
  submit: "生成候选问题",
  submitting: "正在提交请求…",
  retry: "重试提交",
  error: "请求未确认；话题和请求标识已保留，重试不会创建另一条对话。",
  noAccess: "当前角色只能查看问题，不能生成新的问题集。",
  instruction:
    "请根据以下话题生成并保存一组预测的候选问题。覆盖了解、比较和选购等不同意图，使用 question_create 保存，source.kind 为 generated。不要声称它们来自实际用户、搜索量或真实需求。仅生成候选问题时，保存后说明可以编辑这些问题或继续在对话中测量，不自动启动付费测量。如果用户在话题中明确要求测量，则在品类及必要地区足够时，接续 channel_discover、measurement_options、measurement_plan_create 和 measurement_plan_read；不足时只追问一个必要问题。自然推荐问题不加入关注产品或店铺名称；点名了解单独说明，不能当作自然推荐表现。仅在工具确认后提供问题集或实际测量计划链接；受理不代表测量成功，失败时说明原因和下一步。",
  topicData: "话题：{{value}}",
  marketData: "市场：{{value}}",
  languageData: "问题语言：{{value}}",
  routeData: "保存成功后可打开的问题集页面：{{value}}",
} as const;

const en = {
  title: "Suggest questions from a topic",
  description:
    "Enter a word or topic to ask AI for candidate questions you can edit and measure. Predictions are not actual user queries or search volume.",
  topic: "Topic",
  placeholder: "For example: home batteries",
  market: "Market (optional)",
  language: "Question language (optional)",
  marketHint: "Leave blank to use the project setting, or CN if unset.",
  languageHint: "Leave blank to use the project setting, or zh-CN if unset.",
  submit: "Suggest candidate questions",
  submitting: "Sending request…",
  retry: "Retry request",
  error:
    "The request was not confirmed. Your topic and request IDs are kept, so retrying will not start another conversation.",
  noAccess: "Your role can view questions but cannot create a question set.",
  instruction:
    "Generate and save a suggested set of candidate questions from the topic below. Cover exploration, comparison, and choosing intents. Use question_create with source.kind generated. Do not claim these are real user questions, search volume, or observed demand. For a candidate-only request, explain that the saved questions can be edited or measured next in this conversation; do not automatically start paid measurement. If the user explicitly requests measurement in the topic, use channel_discover, measurement_options, measurement_plan_create, and measurement_plan_read once the category and any necessary location are known. Otherwise ask only one necessary question. Do not insert tracked product or store names into unprompted recommendation questions. Label named exploration separately, not as unprompted recommendation performance. Only link a question set or actual measurement plan after its tool receipt confirms it. Acceptance does not mean measurement succeeded; explain failures and the next action.",
  topicData: "Topic: {{value}}",
  marketData: "Market: {{value}}",
  languageData: "Question language: {{value}}",
  routeData: "Question-set page after a successful save: {{value}}",
} as const;

i18n.addResourceBundle("zh-CN", "topicQuestions", zh, true, true);
i18n.addResourceBundle("en", "topicQuestions", en, true, true);
