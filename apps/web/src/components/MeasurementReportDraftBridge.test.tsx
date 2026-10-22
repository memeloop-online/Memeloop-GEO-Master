import { StrictMode } from "react";
import { act, cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import i18n from "../i18n";
import {
  MeasurementReportDraftBridge,
  measurementReportReference,
} from "./MeasurementReportDraftBridge";

const native = vi.hoisted(() => ({
  getState: vi.fn(),
  setText: vi.fn(),
  send: vi.fn(),
}));

vi.mock("@memeloop/react-ui/chat", () => ({
  useAui: () => ({ composer: native }),
}));

const reportId = "11111111-2222-4333-8444-555555555555";

afterEach(async () => {
  cleanup();
  vi.clearAllMocks();
  await i18n.changeLanguage("zh-CN");
});

describe("measurement report draft reference", () => {
  it("accepts only UUID syntax at the URL boundary", () => {
    expect(measurementReportReference(`?measurementReport=${reportId}`)).toBe(
      reportId,
    );
    for (const value of [
      "",
      "synthetic-report",
      `${reportId}/extra`,
      "<script>",
    ]) {
      expect(
        measurementReportReference(
          `?measurementReport=${encodeURIComponent(value)}`,
        ),
      ).toBeUndefined();
    }
    expect(measurementReportReference("?other=value")).toBeUndefined();
  });
});

describe("native measurement report draft bridge", () => {
  it("sets editable native text once, without sending, even after rerenders or language changes", async () => {
    native.getState.mockReturnValue({ text: "" });
    const { rerender, container } = render(
      <StrictMode>
        <MeasurementReportDraftBridge reportId={reportId} />
      </StrictMode>,
    );
    expect(native.setText).toHaveBeenCalledTimes(1);
    expect(native.setText).toHaveBeenCalledWith(
      expect.stringContaining(`编号：${reportId}`),
    );
    rerender(
      <StrictMode>
        <MeasurementReportDraftBridge reportId={reportId} />
      </StrictMode>,
    );
    await act(async () => {
      await i18n.changeLanguage("en");
    });
    expect(native.setText).toHaveBeenCalledTimes(1);
    expect(native.send).not.toHaveBeenCalled();
    expect(container).toBeEmptyDOMElement();
  });

  it.each(["User's own draft", " "])(
    "preserves an existing nonempty native draft: %j",
    (text) => {
      native.getState.mockReturnValue({ text });
      const { rerender } = render(
        <MeasurementReportDraftBridge reportId={reportId} />,
      );
      native.getState.mockReturnValue({ text: "" });
      rerender(<MeasurementReportDraftBridge reportId={reportId} />);
      expect(native.setText).not.toHaveBeenCalled();
      expect(native.send).not.toHaveBeenCalled();
    },
  );

  it("uses the current language for a new handoff", async () => {
    await i18n.changeLanguage("en");
    native.getState.mockReturnValue({ text: "" });
    render(<MeasurementReportDraftBridge reportId={reportId} />);
    expect(native.setText).toHaveBeenCalledWith(
      expect.stringContaining(`report (ID: ${reportId})`),
    );
  });
});
