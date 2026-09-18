export type ProgressState = "complete" | "active" | "blocked" | "queued";

export interface LoopStep {
  id: string;
  label: string;
  state: ProgressState;
  detail: string;
}

export interface OverviewData {
  project: {
    name: string;
    market: string;
    period: string;
    budgetLeft: string;
    budgetDetail: string;
  };
  loop: LoopStep[];
  metrics: Array<{
    label: string;
    value: string;
    detail: string;
    tone?: "positive" | "neutral" | "warning";
  }>;
  trends: Array<{
    platform: string;
    surface: string;
    rate: string;
    change: string;
    sample: string;
    points: number[];
  }>;
  opportunities: Array<{
    title: string;
    detail: string;
    priority: "high" | "medium" | "low";
    action: string;
  }>;
  runs: Array<{
    id: string;
    name: string;
    status: "verified" | "running" | "uncertain" | "blocked";
    detail: string;
    at: string;
  }>;
  causes: Array<{ label: string; count: number; detail: string }>;
}

export const overviewDemo: OverviewData = {
  project: {
    name: "Northstar AI 助手",
    market: "中国大陆 · 简体中文",
    period: "2026.09.01–09.18",
    budgetLeft: "¥ 38,420",
    budgetDetail: "本月预算 ¥ 60,000 · 已预留 ¥ 9,800",
  },
  loop: [
    {
      id: "knowledge",
      label: "知识",
      state: "complete",
      detail: "126 个可用事实",
    },
    {
      id: "questions",
      label: "问题",
      state: "complete",
      detail: "100 个问题已冻结",
    },
    {
      id: "baseline",
      label: "基线",
      state: "complete",
      detail: "450 / 500 有效样本",
    },
    {
      id: "strategy",
      label: "策略",
      state: "complete",
      detail: "本轮计划已生成",
    },
    {
      id: "content",
      label: "内容",
      state: "complete",
      detail: "8 个版本可发布",
    },
    {
      id: "checks",
      label: "检查",
      state: "complete",
      detail: "8 通过 · 1 阻断",
    },
    {
      id: "schedule",
      label: "调度",
      state: "active",
      detail: "3 项等待发送窗口",
    },
    { id: "publish", label: "发布", state: "active", detail: "2 项平台处理中" },
    { id: "verify", label: "验证", state: "queued", detail: "等待公开可见性" },
    {
      id: "remeasure",
      label: "复测",
      state: "queued",
      detail: "9 月 24 日开始",
    },
    {
      id: "optimize",
      label: "优化",
      state: "queued",
      detail: "复测后生成下一轮",
    },
  ],
  metrics: [
    {
      label: "已发布资产",
      value: "14",
      detail: "12 已验证 · 2 平台处理中",
      tone: "positive",
    },
    {
      label: "被引用资产",
      value: "9",
      detail: "仅统计已验证 URL",
      tone: "positive",
    },
    {
      label: "有效 / 计划样本",
      value: "450 / 500",
      detail: "50 个技术缺测，不计作 0",
      tone: "neutral",
    },
    {
      label: "本期成本",
      value: "¥ 11,780",
      detail: "预留 ¥ 9,800 · 已结算 ¥ 1,980",
      tone: "neutral",
    },
    {
      label: "直接线索",
      value: "6",
      detail: "签名 webhook 已确认",
      tone: "positive",
    },
  ],
  trends: [
    {
      platform: "DeepSeek",
      surface: "消费端网页",
      rate: "38.4%",
      change: "+ 6.1%",
      sample: "92 / 100 有效",
      points: [18, 22, 21, 28, 32, 38],
    },
    {
      platform: "Kimi",
      surface: "消费端网页",
      rate: "29.7%",
      change: "+ 3.8%",
      sample: "88 / 100 有效",
      points: [15, 17, 20, 22, 27, 30],
    },
    {
      platform: "Qwen",
      surface: "官方 API 代理",
      rate: "—",
      change: "建立中",
      sample: "基线正在建立，42 / 100 有效",
      points: [],
    },
  ],
  opportunities: [
    {
      title: "补齐“企业 AI 知识库”可引用页面",
      detail: "16 个高价值问题提及了竞品，但没有引用 Northstar 的公开资产。",
      priority: "high",
      action: "创建场景页",
    },
    {
      title: "更新定价事实的结构化数据",
      detail: "官网资料已于 9 月 15 日更新；3 个草稿的检查结论已失效。",
      priority: "high",
      action: "重新检查",
    },
    {
      title: "验证 Kimi 的已发短文",
      detail: "收到平台回执，尚未读回公开页面。",
      priority: "medium",
      action: "查看验证",
    },
  ],
  runs: [
    {
      id: "run-421",
      name: "Kimi · FAQ 变体发布",
      status: "uncertain",
      detail: "平台响应中断，正在对账，不会直接重复发送。",
      at: "14:32",
    },
    {
      id: "run-420",
      name: "DeepSeek · 复测哨兵题",
      status: "running",
      detail: "18 / 20 已完成，2 项等待回答。",
      at: "14:18",
    },
    {
      id: "run-419",
      name: "官网 · 产品页发布验证",
      status: "verified",
      detail: "已读回公开页面并保存证据。",
      at: "13:47",
    },
    {
      id: "run-418",
      name: "比较页事实检查",
      status: "blocked",
      detail: "价格事实有冲突，需先解决知识证据。",
      at: "12:06",
    },
  ],
  causes: [
    { label: "事实冲突", count: 3, detail: "阻断 3 个未发布版本" },
    { label: "验证排队", count: 2, detail: "平台已接收，等待读回" },
    { label: "资源窗口", count: 3, detail: "已排期至下一可用时段" },
  ],
};

export async function getOverviewDemo(): Promise<OverviewData> {
  await new Promise((resolve) => window.setTimeout(resolve, 120));
  return overviewDemo;
}

export const workbenchContent = {
  knowledge: {
    title: "企业知识库",
    eyebrow: "P03 · 知识治理",
    description: "导入企业资料，提取可追溯事实，并让内容检查始终回到来源证据。",
    primary: "导入资料",
    items: [
      "官网内容 · 2026-09-18 更新",
      "产品资料.pdf · 126 个可用事实",
      "FAQ 文档 · 18 个待确认事实",
    ],
  },
  source: {
    title: "来源与解析结果",
    eyebrow: "P04 · 来源证据",
    description:
      "查看来源版本、解析结果、可定位证据，以及被哪些事实与内容使用。",
    primary: "重新解析",
    items: [
      "文档版本 v4 · 解析完成",
      "定位证据：第 4 页，第 2 段",
      "使用它的内容：产品页 v7、FAQ v3",
    ],
  },
  ask: {
    title: "企业问答",
    eyebrow: "P05 · 问题探索",
    description: "从已授权的企业知识中探索问题；回答只引用可定位的证据。",
    primary: "新建问题",
    items: [
      "推荐提问：Northstar 支持哪些系统？",
      "可将选定证据转换为 Content Brief",
      "最多带入 12 个证据片段",
    ],
  },
  campaigns: {
    title: "优化计划",
    eyebrow: "P06 · 计划列表",
    description:
      "按目标、市场和周期组织机会、实验与预算，已开始的动作不会被隐式取消。",
    primary: "创建计划",
    items: [
      "9 月增长计划 · 运行中",
      "企业知识库机会 · 8 个待办动作",
      "竞争格局复测 · 将于 9 月 24 日开始",
    ],
  },
  campaign: {
    title: "优化计划与动作",
    eyebrow: "P07 · 计划详情",
    description: "查看基线证据、机会排序、动作时间线、实验组与终止条件。",
    primary: "启动本轮",
    items: [
      "目标：Northstar AI 助手",
      "预算：¥ 20,000 · 已预留 ¥ 6,400",
      "动作：3 项排期，1 项阻断",
    ],
  },
  content: {
    title: "内容资产",
    eyebrow: "P08 · 内容列表",
    description:
      "管理 Master Content、渠道变体和版本检查；人工编辑会使检查结论失效。",
    primary: "新建内容",
    items: [
      "企业 AI 知识库场景页 · 检查通过",
      "产品 FAQ · 已验证发布",
      "竞品比较页 · 事实冲突",
    ],
  },
  contentDetail: {
    title: "内容编辑器",
    eyebrow: "P09 · 内容与证据",
    description:
      "编辑正文、渠道变体与结构化字段，同时核对事实来源、冲突和修复记录。",
    primary: "保存草稿",
    items: [
      "自动保存：空闲 1 秒后",
      "证据：12 个可定位片段",
      "检查：关键事实缺失会阻断发布",
    ],
  },
  channels: {
    title: "渠道与资源池",
    eyebrow: "P10 · 账号、执行与出口",
    description: "管理客户账号、总部资源、执行环境和稳定的账号出口绑定。",
    primary: "接入账号",
    items: [
      "客户账号池 · 8 个健康账号",
      "总部资源池 · 16 个可用额度",
      "出口池 · 3 个同地区健康端点",
    ],
  },
  connect: {
    title: "批量接入渠道",
    eyebrow: "P11 · 连接配置",
    description:
      "按渠道定义字段批量导入、脱敏预览并建立会话；重复账号默认跳过。",
    primary: "上传文件",
    items: ["1. 上传或粘贴", "2. 映射字段并校验", "3. 查看逐项导入结果"],
  },
  publications: {
    title: "发布与验证",
    eyebrow: "P12 · 任务、回执与对账",
    description:
      "严格区分已发布与已验证。结果待确认时系统只对账，不会直接重复发送。",
    primary: "查看排期",
    items: [
      "2 项平台处理中",
      "1 项结果待确认，正在对账",
      "12 项公开可见性已验证",
    ],
  },
  measurement: {
    title: "基线与周期测量",
    eyebrow: "P13 · 多观测面",
    description: "按平台、观测面、问题版本和有效样本展示测量；缺测显示为“—”。",
    primary: "创建测量",
    items: [
      "消费端网页 · 180 / 200 有效",
      "消费端 APP · 尚未配置",
      "官方 API 代理 · 正在建立基线",
    ],
  },
  reports: {
    title: "效果报告",
    eyebrow: "P14 · 证据与线索",
    description:
      "用不可变快照回看变化、原始证据与已确认线索，不从访问量推断收入。",
    primary: "导出报告",
    items: ["2026 W38 报告 · 已生成", "引用率按平台分段", "6 条直接线索已确认"],
  },
  billing: {
    title: "预算、用量与账本",
    eyebrow: "P15 · 成本控制",
    description: "核对预算预留、实际费用和结算；预算暂停会阻止新的付费任务。",
    primary: "查看账本",
    items: ["可用预算：¥ 38,420", "本期已结算：¥ 1,980", "测量预算保留：20%"],
  },
  settings: {
    title: "项目设置",
    eyebrow: "P16 · 规则与成员",
    description: "维护项目市场、自动化规则、通知、数据策略与成员权限。",
    primary: "保存设置",
    items: ["自动优化：运行中", "规则版本：v3", "成员：6 位，1 位只读成员"],
  },
} as const;

export type WorkbenchPageKey = keyof typeof workbenchContent;
