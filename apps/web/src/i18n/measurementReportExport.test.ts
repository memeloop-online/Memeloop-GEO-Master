import { describe, expect, it } from "vitest";
import i18n from "./index";
import { measurementReportExportTranslations } from "./measurementReportExport";

describe("measurement export translations", () => {
  it("registers complete nonempty English and Chinese resources", () => {
    const { en, "zh-CN": zh } = measurementReportExportTranslations;
    expect(Object.keys(en).sort()).toEqual(Object.keys(zh).sort());
    for (const locale of ["en", "zh-CN"] as const) {
      for (const [key, value] of Object.entries(
        measurementReportExportTranslations[locale],
      )) {
        expect(value.trim()).not.toBe("");
        expect(i18n.getFixedT(locale, "measurementReportExport")(key)).toBe(
          value,
        );
      }
    }
    expect(en.downloadCsv).toBe("Download measurement CSV");
    expect(zh.downloadCsv).toBe("下载测量 CSV");
  });
});
