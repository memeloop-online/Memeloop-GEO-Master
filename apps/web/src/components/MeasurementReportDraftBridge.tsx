import { useEffect, useRef } from "react";
import { useAui } from "@memeloop/react-ui/chat";
import { useTranslation } from "react-i18next";
import "../i18n/measurementReportChat";
import type { RecommendationItem } from "../api/recommendations";

const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export interface MeasurementRecommendationReference {
  attemptId: string | null;
  kind: RecommendationItem["kind"];
  mentionOrder: number;
}

function selectedRecommendation(params: URLSearchParams) {
  const attempt = params.get("measurementAttempt");
  const kind = params.get("measurementRecommendationKind");
  const order = params.get("measurementRecommendationOrder");
  if (attempt === null && kind === null && order === null) return undefined;
  if (
    !params.get("measurementQuestionTarget") ||
    !attempt ||
    (attempt !== "none" && !uuid.test(attempt)) ||
    !kind ||
    !["product", "restaurant", "brand", "service", "other"].includes(kind) ||
    !order ||
    !/^[1-9]\d*$/.test(order) ||
    !Number.isSafeInteger(Number(order))
  )
    return null;
  return {
    attemptId: attempt === "none" ? null : attempt,
    kind: kind as RecommendationItem["kind"],
    mentionOrder: Number(order),
  };
}

export function measurementReportReference(search: string) {
  const params = new URLSearchParams(search);
  const reportId = params.get("measurementReport");
  const questionTarget = params.get("measurementQuestionTarget");
  return reportId &&
    uuid.test(reportId) &&
    (questionTarget === null || uuid.test(questionTarget)) &&
    selectedRecommendation(params) !== null
    ? reportId
    : undefined;
}

export function measurementRecommendationReference(search: string) {
  if (!measurementReportReference(search)) return undefined;
  return selectedRecommendation(new URLSearchParams(search)) ?? undefined;
}

export function measurementQuestionTargetReference(search: string) {
  if (!measurementReportReference(search)) return undefined;
  return (
    new URLSearchParams(search).get("measurementQuestionTarget") ?? undefined
  );
}

// Mounted within the native composer; the user reviews and sends this draft.
export function MeasurementReportDraftBridge({
  reportId,
  questionTargetId,
  recommendation,
}: {
  reportId: string;
  questionTargetId?: string;
  recommendation?: MeasurementRecommendationReference;
}) {
  const aui = useAui();
  const { t } = useTranslation("measurementReportChat");
  const applied = useRef(false);

  useEffect(() => {
    if (applied.current) return;
    applied.current = true;
    if (aui.composer.getState().text !== "") return;
    aui.composer.setText(
      t(recommendation ? "recommendationRequest" : "request", { reportId }) +
        (recommendation
          ? ` ${t("selectedRecommendation", {
              targetId: questionTargetId,
              attemptId: recommendation.attemptId ?? "null",
              kind: recommendation.kind,
              mentionOrder: recommendation.mentionOrder,
            })}`
          : questionTargetId
            ? ` ${t("selectedQuestion", { questionTargetId })}`
            : ""),
    );
  }, [aui, reportId, questionTargetId, recommendation, t]);

  return null;
}
