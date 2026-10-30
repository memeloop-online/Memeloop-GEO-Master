import i18n from "./index";

const translations = {
  "zh-CN": {
    request:
      "分析这次测量，看看我的产品表现，以及值得优先补充的内容和渠道。测量记录：{{planId}}",
  },
  en: {
    request:
      "Analyze this measurement: how did my product perform, and which content and channels should I prioritize? Measurement record: {{planId}}",
  },
};

for (const [language, translation] of Object.entries(translations)) {
  i18n.addResourceBundle(
    language,
    "measurementPlanChat",
    translation,
    true,
    true,
  );
}
