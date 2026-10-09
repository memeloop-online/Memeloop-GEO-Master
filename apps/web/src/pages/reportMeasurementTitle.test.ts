import { afterEach, describe, expect, it } from "vitest";
import i18n from "../i18n";
import { reportMeasurementTitle } from "./reportMeasurementTitle";

afterEach(async () => {
  await i18n.changeLanguage("zh-CN");
});
describe("report measurement labels", () => {
  it("uses only the saved provider and model in known comparison formats", () => {
    const key = "provider-a|model-a|consumer_web|web_search|v1|ad_hoc.v1|US|en";
    expect(reportMeasurementTitle(key)).toBe("provider-a · model-a");
    expect(reportMeasurementTitle(`${key}|frozen_evaluation|split-v1`)).toBe(
      "provider-a · model-a",
    );
  });
  it("does not invent question or provider labels for unknown keys", async () => {
    expect(reportMeasurementTitle("opaque-key")).toBe("测量记录");
    expect(
      reportMeasurementTitle("provider||web|search|v1|questions|US|en"),
    ).toBe("测量记录");
    await i18n.changeLanguage("en");
    expect(reportMeasurementTitle("opaque-key")).toBe("Measurement record");
  });
});
