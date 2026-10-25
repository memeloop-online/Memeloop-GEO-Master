import { describe, expect, it } from "vitest";
import i18n from "./index";
import { measurementReportPdfTranslations } from "./measurementReportPdf";

describe("measurement PDF translations", () => {
  it("registers matching, nonempty English and Chinese resources", () => {
    const { en, "zh-CN": zh } = measurementReportPdfTranslations;
    expect(Object.keys(en).sort()).toEqual(Object.keys(zh).sort());
    for (const locale of ["en", "zh-CN"] as const) {
      for (const [key, value] of Object.entries(
        measurementReportPdfTranslations[locale],
      )) {
        expect(value.trim()).not.toBe("");
        expect(i18n.getResource(locale, "measurementReportPdf", key)).toBe(
          value,
        );
        expect(value.match(/\{\{\w+\}\}/gu)?.sort()).toEqual(
          en[key as keyof typeof en].match(/\{\{\w+\}\}/gu)?.sort(),
        );
      }
    }
    expect(en.downloadPdf).toBe("Download PDF");
    expect(zh.downloadPdf).toBe("下载 PDF");
  });
});
