import i18n from "./index";

const translations = {
  "zh-CN": {
    title: "关注对象与同类推荐",
    scope: "查看本次回答中的推荐理由与原文依据。",
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
    title: "Tracked item and similar recommendations",
    scope:
      "Review recommendation reasons and supporting excerpts from this answer.",
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
