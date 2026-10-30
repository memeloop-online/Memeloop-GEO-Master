import i18n from "./index";

const zh = {
  title: "客户问 AI 时，会推荐你的产品或店铺吗？",
  description: "输入产品、店铺或品类，查看 AI 推荐、评价、竞争对象和引用信源。",
  hint: "选择示例，修改后发送；也可以直接提问。",
  preserved: "已保留你正在编辑的内容，可继续修改后发送。",
  placeholder: "输入你的产品、店铺或想测量的问题…",
  examples: {
    product: {
      title: "我的产品会被推荐吗？",
      detail: "查看选购推荐中是否出现、推荐理由和明确排名",
      draft:
        "我想测量【产品名称】在【品类】选购推荐中的表现。请用不点名产品的顾客问题，看看 AI 会推荐谁、是否包含我的产品，以及推荐理由和明确排名。",
    },
    shop: {
      title: "顾客找店时会发现我吗？",
      detail: "按城市和店铺类型，查看 AI 推荐哪些店",
      draft:
        "我的店是【城市】的【店铺名称】，经营【店铺类型】。请测量顾客找这类店时 AI 会推荐谁，以及我的店是否出现；提问时不要点名我的店。",
    },
    perception: {
      title: "AI 如何评价我的产品？",
      detail: "点名了解，查看介绍、优缺点和依据",
      draft:
        "请测量 AI 如何介绍和评价【产品名称】，查看它给出的优点、局限和引用依据。这次是点名了解，不作为自然推荐表现。",
    },
    sources: {
      title: "竞争推荐来自哪些信源？",
      detail: "查看被推荐的对象、引用文章和可经营的渠道",
      draft:
        "请测量【品类】选购问题，看看 AI 推荐哪些产品、引用哪些文章和网站，并根据实际引用给我内容渠道建议。",
    },
  },
} as const;

const en = {
  title: "Will AI recommend your product or store?",
  description:
    "Explore AI recommendations, descriptions, competing options, and cited sources.",
  hint: "Choose an example to edit, or ask your own question.",
  preserved:
    "Your draft has been kept. You can continue editing before sending.",
  placeholder: "Enter your product, store, or a question to measure…",
  examples: {
    product: {
      title: "Will AI recommend my product?",
      detail: "Check inclusion, recommendation reasons, and explicit rankings",
      draft:
        "Measure how [product name] appears in buying recommendations for [category]. Use customer questions that do not name my product, and show which products AI recommends, whether mine appears, the reasons, and any explicit rankings.",
    },
    shop: {
      title: "Will customers discover my store?",
      detail: "See which stores AI recommends for a city and category",
      draft:
        "My store is [store name], a [store type] in [city]. Measure which stores AI recommends when customers look for this kind of store and whether mine appears, without naming my store in the questions.",
    },
    perception: {
      title: "How does AI describe my product?",
      detail: "Ask by name to examine strengths, limitations, and sources",
      draft:
        "Measure how AI describes and evaluates [product name], including its strengths, limitations, and cited sources. This is a named question, not a test of unprompted recommendations.",
    },
    sources: {
      title: "Which sources support competing recommendations?",
      detail: "Find recommended products, cited pages, and content channels",
      draft:
        "Measure buying questions about [category]. Show which products AI recommends and which articles and websites it cites, then suggest content channels based on those actual citations.",
    },
  },
} as const;

i18n.addResourceBundle("zh-CN", "chatMeasurementGuidance", zh, true, true);
i18n.addResourceBundle("en", "chatMeasurementGuidance", en, true, true);
