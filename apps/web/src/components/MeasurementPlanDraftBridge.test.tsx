import { StrictMode } from "react";
import { act, cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import i18n from "../i18n";
import {
  MeasurementPlanDraftBridge,
  measurementPlanReference,
} from "./MeasurementPlanDraftBridge";

const native = vi.hoisted(() => ({
  getState: vi.fn(),
  setText: vi.fn(),
  send: vi.fn(),
}));

vi.mock("@memeloop/react-ui/chat", () => ({
  useAui: () => ({ composer: native }),
}));

const planId = "11111111-2222-4333-8444-555555555555";

afterEach(async () => {
  cleanup();
  vi.clearAllMocks();
  await i18n.changeLanguage("zh-CN");
});

describe("measurement plan URL reference", () => {
  it("accepts one stable plan UUID without importing other URL content", () => {
    expect(measurementPlanReference(`?measurementPlan=${planId}`)).toBe(planId);
    expect(
      measurementPlanReference(
        `?measurementPlan=${planId}&draft=untrusted-text&account=untrusted-id`,
      ),
    ).toBe(planId);
  });

  it.each(["", "synthetic-plan", `${planId}/extra`, "<script>", " "])(
    "rejects an invalid plan reference: %j",
    (value) => {
      const search = `?measurementPlan=${encodeURIComponent(value)}`;
      expect(measurementPlanReference(search)).toBeUndefined();
      native.getState.mockReturnValue({ text: "" });
      const selectedPlanId = measurementPlanReference(search);
      render(
        selectedPlanId ? (
          <MeasurementPlanDraftBridge planId={selectedPlanId} />
        ) : null,
      );
      expect(native.setText).not.toHaveBeenCalled();
      expect(native.send).not.toHaveBeenCalled();
    },
  );

  it("rejects missing, duplicate, or mixed measurement scopes", () => {
    for (const search of [
      "",
      "?other=value",
      `?measurementPlan=${planId}&measurementPlan=${planId}`,
      `?measurementPlan=${planId}&measurementReport=${planId}`,
      `?measurementPlan=${planId}&measurementQuestionTarget=${planId}`,
      `?measurementPlan=${planId}&measurementAttempt=${planId}`,
      `?measurementPlan=${planId}&measurementRecommendationKind=product`,
      `?measurementPlan=${planId}&measurementRecommendationOrder=1`,
    ])
      expect(measurementPlanReference(search)).toBeUndefined();
  });
});

describe("native measurement plan draft bridge", () => {
  it.each(["zh-CN", "en"])(
    "offers a concise scoped editable request in %s without sending",
    async (language) => {
      await i18n.changeLanguage(language);
      native.getState.mockReturnValue({ text: "" });
      const { container } = render(
        <MeasurementPlanDraftBridge planId={planId} />,
      );
      const draft = native.setText.mock.calls[0][0] as string;
      expect(draft).toBe(i18n.t("measurementPlanChat:request", { planId }));
      expect(draft).toContain(planId);
      for (const phrase of language === "en"
        ? ["my product perform", "content and channels", "Measurement record:"]
        : ["我的产品表现", "优先补充的内容和渠道", "测量记录："])
        expect(draft).toContain(phrase);
      expect(draft).not.toMatch(
        /measurement_plan_read|frozen_evaluation|冻结评估|schema|systemPrompt/,
      );
      expect(draft.length).toBeLessThanOrEqual(language === "en" ? 180 : 100);
      expect(native.send).not.toHaveBeenCalled();
      expect(container).toBeEmptyDOMElement();
    },
  );

  it("fills only once through StrictMode, rerenders, and language changes", async () => {
    native.getState.mockReturnValue({ text: "" });
    const { rerender } = render(
      <StrictMode>
        <MeasurementPlanDraftBridge planId={planId} />
      </StrictMode>,
    );
    rerender(
      <StrictMode>
        <MeasurementPlanDraftBridge planId={planId} />
      </StrictMode>,
    );
    await act(async () => {
      await i18n.changeLanguage("en");
    });
    expect(native.setText).toHaveBeenCalledTimes(1);
    expect(native.send).not.toHaveBeenCalled();
  });

  it.each(["My existing draft", " "])(
    "preserves an existing draft and does not refill after it is cleared: %j",
    (text) => {
      native.getState.mockReturnValue({ text });
      const { rerender } = render(
        <MeasurementPlanDraftBridge planId={planId} />,
      );
      native.getState.mockReturnValue({ text: "" });
      rerender(<MeasurementPlanDraftBridge planId={planId} />);
      expect(native.setText).not.toHaveBeenCalled();
      expect(native.send).not.toHaveBeenCalled();
    },
  );
});
