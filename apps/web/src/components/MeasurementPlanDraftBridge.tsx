import { useEffect, useRef } from "react";
import { useAui } from "@memeloop/react-ui/chat";
import { useTranslation } from "react-i18next";
import "../i18n/measurementPlanChat";

const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export function measurementPlanReference(search: string) {
  const params = new URLSearchParams(search);
  const planIds = params.getAll("measurementPlan");
  if (
    planIds.length !== 1 ||
    !uuid.test(planIds[0]) ||
    [...params.keys()].some(
      (key) => key.startsWith("measurement") && key !== "measurementPlan",
    )
  )
    return undefined;
  return planIds[0];
}

// Mounted within the native composer; sending remains an explicit user action.
export function MeasurementPlanDraftBridge({ planId }: { planId: string }) {
  const aui = useAui();
  const { t } = useTranslation("measurementPlanChat");
  const applied = useRef(false);

  useEffect(() => {
    if (applied.current) return;
    applied.current = true;
    if (aui.composer.getState().text !== "") return;
    aui.composer.setText(t("request", { planId }));
  }, [aui, planId, t]);

  return null;
}
