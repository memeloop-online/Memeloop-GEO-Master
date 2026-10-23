import { StrictMode } from "react";
import { act, cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import i18n from "../i18n";
import {
  MeasurementReportDraftBridge,
  measurementReportReference,
  measurementQuestionTargetReference,
  measurementRecommendationReference,
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
const questionTargetId = "66666666-7777-4888-8999-000000000000";
const attemptId = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

afterEach(async () => {
  cleanup();
  vi.clearAllMocks();
  await i18n.changeLanguage("zh-CN");
});

describe("measurement report draft reference", () => {
  it("keeps the exact sample, attempt and occurrence pointer and rejects incomplete pointers", () => {
    const base = `?measurementReport=${reportId}&measurementQuestionTarget=${questionTargetId}`;
    const occurrence = `&measurementAttempt=${attemptId}&measurementRecommendationKind=product&measurementRecommendationOrder=3`;
    expect(measurementRecommendationReference(base + occurrence)).toEqual({
      attemptId,
      kind: "product",
      mentionOrder: 3,
    });
    expect(
      measurementRecommendationReference(
        base + occurrence.replace(attemptId, "none"),
      ),
    ).toEqual({ attemptId: null, kind: "product", mentionOrder: 3 });
    for (const search of [
      `?measurementReport=${reportId}${occurrence}`,
      `${base}&measurementRecommendationOrder=3`,
      base + occurrence.replace(attemptId, "invalid"),
      base + occurrence.replace("Kind=product", "Kind=unknown"),
      base + occurrence.replace("Order=3", "Order=0"),
      base + occurrence.replace("Order=3", "Order=1.5"),
    ]) {
      expect(measurementReportReference(search)).toBeUndefined();
      expect(measurementRecommendationReference(search)).toBeUndefined();
    }
  });
  it("accepts a question sample UUID only with a valid report and never widens an invalid selected scope", () => {
    const search = `?measurementReport=${reportId}&measurementQuestionTarget=${questionTargetId}`;
    expect(measurementReportReference(search)).toBe(reportId);
    expect(measurementQuestionTargetReference(search)).toBe(questionTargetId);
    expect(
      measurementQuestionTargetReference(
        `?measurementQuestionTarget=${questionTargetId}`,
      ),
    ).toBeUndefined();
    for (const value of ["", "invalid", `${questionTargetId}/extra`]) {
      const invalid = `?measurementReport=${reportId}&measurementQuestionTarget=${encodeURIComponent(value)}`;
      expect(measurementReportReference(invalid)).toBeUndefined();
      expect(measurementQuestionTargetReference(invalid)).toBeUndefined();
    }
  });
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
  it.each(["zh-CN", "en"])(
    "keeps a concise editable request with the exact saved occurrence in %s",
    async (language) => {
      await i18n.changeLanguage(language);
      native.getState.mockReturnValue({ text: "" });
      render(
        <MeasurementReportDraftBridge
          reportId={reportId}
          questionTargetId={questionTargetId}
          recommendation={{ attemptId, kind: "product", mentionOrder: 3 }}
        />,
      );
      const draft = native.setText.mock.calls[0][0] as string;
      for (const value of [
        reportId,
        `target_id=${questionTargetId}`,
        `attempt_id=${attemptId}`,
        "kind=product",
        "mention_order=3",
      ])
        expect(draft).toContain(value);
      expect(draft).toContain(
        language === "en"
          ? "Separate evidence from hypotheses"
          : "区分证据与假设",
      );
      expect(draft).toContain(
        language === "en" ? "unread linked pages" : "未读取的引用页面",
      );
      expect(draft).toContain(
        language === "en" ? "its use is restricted" : "使用受限时说明",
      );
      expect(draft).not.toMatch(
        /report\.get|question_binding|frozen_evaluation|evidence_quote/,
      );
      if (language === "zh-CN") expect(draft.length).toBeLessThanOrEqual(500);
      expect(native.send).not.toHaveBeenCalled();
    },
  );
  it.each(["zh-CN", "en"])(
    "keeps the report and selected question sample separate in an editable %s draft",
    async (language) => {
      await i18n.changeLanguage(language);
      native.getState.mockReturnValue({ text: "" });
      render(
        <MeasurementReportDraftBridge
          reportId={reportId}
          questionTargetId={questionTargetId}
        />,
      );
      expect(native.setText).toHaveBeenCalledWith(
        expect.stringContaining(reportId),
      );
      expect(native.setText).toHaveBeenCalledWith(
        expect.stringContaining(
          language === "en"
            ? `selected question (sample ID: ${questionTargetId})`
            : `所选问题（样本编号：${questionTargetId}）`,
        ),
      );
      expect(native.send).not.toHaveBeenCalled();
    },
  );
  it("sets editable native text once, without sending, even after rerenders or language changes", async () => {
    native.getState.mockReturnValue({ text: "" });
    const { rerender, container } = render(
      <StrictMode>
        <MeasurementReportDraftBridge reportId={reportId} />
      </StrictMode>,
    );
    expect(native.setText).toHaveBeenCalledTimes(1);
    expect(native.setText).toHaveBeenCalledWith(
      i18n.t("measurementReportChat:request", { reportId }),
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
        <MeasurementReportDraftBridge
          reportId={reportId}
          questionTargetId={questionTargetId}
          recommendation={{ attemptId, kind: "product", mentionOrder: 3 }}
        />,
      );
      native.getState.mockReturnValue({ text: "" });
      rerender(
        <MeasurementReportDraftBridge
          reportId={reportId}
          questionTargetId={questionTargetId}
          recommendation={{ attemptId, kind: "product", mentionOrder: 3 }}
        />,
      );
      expect(native.setText).not.toHaveBeenCalled();
      expect(native.send).not.toHaveBeenCalled();
    },
  );

  it("uses the current language for a new handoff", async () => {
    await i18n.changeLanguage("en");
    native.getState.mockReturnValue({ text: "" });
    render(<MeasurementReportDraftBridge reportId={reportId} />);
    expect(native.setText).toHaveBeenCalledWith(
      expect.stringContaining(
        `independent measurement report (ID: ${reportId})`,
      ),
    );
  });
});
