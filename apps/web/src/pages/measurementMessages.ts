import i18n from "../i18n";

const zh = {
  measure: "开始测量",
  records: "测量记录",
  insights: "信源洞察",
  sets: "问题集",
  newSet: "新建问题集",
  hideNewSet: "收起新建",
  questionsHeading: "问题集与版本",
  questionsIntro: "保存常用问题，查看历史版本，或修订当前版本。",
  pageIntro: "输入问题并选择账号，即可开始测量。",
  prediction: "只有话题？让 AI 帮你想问题",
  optionalRegion: "市场与语言（可选）",
  createSet: "创建并保存问题集",
  creatingSet: "正在创建…",
  moreSettings: "更多设置",
} as const;

const en = {
  measure: "Start measurement",
  records: "Measurement records",
  insights: "Citation insights",
  sets: "Question sets",
  newSet: "New question set",
  hideNewSet: "Close new set",
  questionsHeading: "Question sets and versions",
  questionsIntro:
    "Save recurring questions, inspect previous versions, or revise the current version.",
  pageIntro: "Enter a question and choose an account to begin.",
  prediction: "Only have a topic? Ask AI for questions",
  optionalRegion: "Market and language (optional)",
  createSet: "Create question set",
  creatingSet: "Creating…",
  moreSettings: "More settings",
} as const;

i18n.addResourceBundle("zh-CN", "measurement", zh, true, true);
i18n.addResourceBundle("en", "measurement", en, true, true);
