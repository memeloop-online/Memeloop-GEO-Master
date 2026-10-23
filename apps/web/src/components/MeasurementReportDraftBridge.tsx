import { useEffect, useRef } from "react";
import { useAui } from "@memeloop/react-ui/chat";
import { useTranslation } from "react-i18next";
import "../i18n/measurementReportChat";

const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export function measurementReportReference(search: string) {
  const params = new URLSearchParams(search);
  const reportId = params.get("measurementReport");
  const questionTarget = params.get("measurementQuestionTarget");
  return reportId &&
    uuid.test(reportId) &&
    (questionTarget === null || uuid.test(questionTarget))
    ? reportId
    : undefined;
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
}: {
  reportId: string;
  questionTargetId?: string;
}) {
  const aui = useAui();
  const { t } = useTranslation("measurementReportChat");
  const applied = useRef(false);

  useEffect(() => {
    if (applied.current) return;
    applied.current = true;
    if (aui.composer.getState().text !== "") return;
    aui.composer.setText(
      t("request", { reportId }) +
        (questionTargetId
          ? ` ${t("selectedQuestion", { questionTargetId })}`
          : ""),
    );
  }, [aui, reportId, questionTargetId, t]);

  return null;
}
