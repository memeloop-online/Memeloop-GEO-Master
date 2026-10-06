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
    "请根据以下话题生成并保存一组预测的候选问题。覆盖了解、比较和选购等不同意图，使用 question_create 保存，source.kind 为 generated。不要声称它们来自实际用户、搜索量或真实需求，不要启动测量。仅在工具确认保存后提供问题集链接；如果无法保存，请明确说明。",
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
    "Generate and save a suggested set of candidate questions from the topic below. Cover exploration, comparison, and choosing intents. Use question_create with source.kind generated. Do not claim these are real user questions, search volume, or observed demand; do not start measurement. Only provide a question-set link after a tool receipt confirms the save. If saving fails, say so clearly.",
  topicData: "Topic: {{value}}",
  marketData: "Market: {{value}}",
  languageData: "Question language: {{value}}",
  routeData: "Question-set page after a successful save: {{value}}",
} as const;

i18n.addResourceBundle("zh-CN", "topicQuestions", zh, true, true);
i18n.addResourceBundle("en", "topicQuestions", en, true, true);
