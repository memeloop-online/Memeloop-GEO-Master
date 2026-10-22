import { useEffect, useRef } from "react";
import { useAui } from "@memeloop/react-ui/chat";
import { useTranslation } from "react-i18next";
import "../i18n/measurementReportChat";

export function measurementReportReference(search: string) {
  const reportId = new URLSearchParams(search).get("measurementReport");
  return reportId &&
    /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(
      reportId,
    )
    ? reportId
    : undefined;
}

// Mounted within the native composer; the user reviews and sends this draft.
export function MeasurementReportDraftBridge({
  reportId,
}: {
  reportId: string;
}) {
  const aui = useAui();
  const { t } = useTranslation("measurementReportChat");
  const applied = useRef(false);

  useEffect(() => {
    if (applied.current) return;
    applied.current = true;
    if (aui.composer.getState().text !== "") return;
    aui.composer.setText(t("request", { reportId }));
  }, [aui, reportId, t]);

  return null;
}
