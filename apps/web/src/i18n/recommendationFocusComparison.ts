import i18n from "./index";

const translations = {
  "zh-CN": {
    analyzeImprovements: "分析改进方向",
    selectionUnavailable: "暂无法定位这条推荐的已保存证据，请刷新后重试。",
    title: "关注对象与同类推荐",
    scope: "查看本次回答中的推荐理由与原文依据。",
    recommendedTitle: "本次回答推荐了什么",
    recommendedScope:
      "以下内容展示本次回答中的推荐理由与原文依据，并不解释关注对象为何未被推荐。",
    fullList: "查看完整推荐列表",
    group: "{{name}} · {{kind}}",
    focused: "关注对象",
    ranked: "推荐第 {{rank}} 位",
    unranked: "已推荐，未明确排名",
    ahead: "排名更靠前的推荐",
    noneAhead: "本次同类推荐中，没有明确排名更靠前的对象。",
    cannotCompare: "本次回答未给出该对象的明确排名，暂无法比较先后。",
    ambiguous: "同名同类型出现不同排名，暂无法比较先后。",
    reasonMissing: "未说明推荐理由",
    evidence: "查看原文依据与引用",
    kind_product: "产品",
    kind_restaurant: "餐馆",
    kind_brand: "品牌",
    kind_service: "服务",
    kind_other: "其他对象",
  },
  en: {
    analyzeImprovements: "Explore improvements",
    selectionUnavailable:
      "This recommendation's saved evidence cannot be located. Refresh and try again.",
    title: "Tracked item and similar recommendations",
    scope:
      "Review recommendation reasons and supporting excerpts from this answer.",
    recommendedTitle: "What this answer recommended",
    recommendedScope:
      "These are the recommendation reasons and supporting excerpts from this answer; they do not explain why the tracked item was not recommended.",
    fullList: "View the full recommendation list",
    group: "{{name}} · {{kind}}",
    focused: "Tracked item",
    ranked: "Recommended at #{{rank}}",
    unranked: "Recommended without an explicit rank",
    ahead: "Recommendations ranked ahead",
    noneAhead: "No similar item has an explicit rank ahead in this answer.",
    cannotCompare:
      "This answer gives no explicit rank for this item, so ranks cannot be compared.",
    ambiguous:
      "Results with this name and type have conflicting ranks, so ranks cannot be compared.",
    reasonMissing: "No recommendation reason given",
    evidence: "View supporting excerpt and citations",
    kind_product: "Product",
    kind_restaurant: "Restaurant",
    kind_brand: "Brand",
    kind_service: "Service",
    kind_other: "Other item",
  },
};

for (const [language, translation] of Object.entries(translations)) {
  i18n.addResourceBundle(
    language,
    "recommendationFocusComparison",
    translation,
    true,
    true,
  );
}
