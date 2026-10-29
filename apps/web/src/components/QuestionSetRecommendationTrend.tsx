import { useState } from "react";
import { Button } from "@fluentui/react-components";
import { useTranslation } from "react-i18next";
import { useSearchParams } from "react-router-dom";
import { MeasurementPeriodReportPanel } from "../pages/MeasurementPeriodReportPanel";
import "../i18n/questionSetRecommendationTrend";

export function QuestionSetRecommendationTrend({
  tenantId,
  projectId,
  questionSetId,
  questionSetVersionId,
  initiallyExpanded = false,
  onPreview,
  embedded = false,
  showFocusSettings = true,
}: {
  tenantId: string;
  projectId: string;
  questionSetId: string;
  questionSetVersionId: string;
  initiallyExpanded?: boolean;
  onPreview?: () => void;
  embedded?: boolean;
  showFocusSettings?: boolean;
}) {
  const { t } = useTranslation("questionSetRecommendationTrend");
  const [searchParams, setSearchParams] = useSearchParams();
  const [expanded, setExpanded] = useState(initiallyExpanded);
  const selectedReportId =
    (searchParams.get("set") ?? searchParams.get("questionSetId")) ===
      questionSetId &&
    (searchParams.get("version") ?? searchParams.get("versionId")) ===
      questionSetVersionId
      ? searchParams.get("rankReport") || undefined
      : undefined;
  const open = expanded || Boolean(selectedReportId);
  const selectReport = (reportId: string | undefined) => {
    setExpanded(true);
    if (!reportId) onPreview?.();
    setSearchParams((previous) => {
      const next = new URLSearchParams(previous);
      if (reportId) {
        next.set("set", questionSetId);
        next.set("version", questionSetVersionId);
        next.set("rankReport", reportId);
      } else if (selectedReportId) next.delete("rankReport");
      return next;
    });
  };
  return (
    <div>
      <Button
        appearance="subtle"
        aria-expanded={open}
        onClick={() => {
          if (open && selectedReportId) selectReport(undefined);
          setExpanded(!open);
        }}
      >
        {t(open ? "close" : "open")}
      </Button>
      {open && (
        <MeasurementPeriodReportPanel
          key={`${tenantId}/${projectId}/${questionSetId}/${questionSetVersionId}`}
          tenantId={tenantId}
          projectId={projectId}
          selectedReportId={selectedReportId}
          onSelectReport={selectReport}
          embedded={embedded}
          showFocusSettings={showFocusSettings}
          questionSet={{
            question_set_id: questionSetId,
            question_set_version_id: questionSetVersionId,
          }}
        />
      )}
    </div>
  );
}
