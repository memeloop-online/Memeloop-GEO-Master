import { useRef, useState } from "react";
import { Button, Card } from "@fluentui/react-components";
import { useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { useAuth } from "../auth/AuthProvider";
import {
  getMeasurementReport,
  getMeasurementReportPreview,
  listMeasurementReports,
  saveMeasurementReport,
  type MeasurementPeriodProjection,
} from "../api/measurementReports";
import { ReportSupplementaryMeasurements } from "./ReportSupplementaryMeasurements";
import { safeOriginalPublicUrl } from "./PublicationLookupPanel";
import "../i18n/measurementReports";
import { reportMeasurementTitle } from "./reportMeasurementTitle";
import { MeasurementReportSearchSection } from "./MeasurementReportSearchSection";

const knownStatuses = new Set([
  "pending",
  "unknown",
  "observed",
  "refused",
  "missing",
  "failed",
  "login_required",
  "unsupported",
]);

export function MeasurementPeriodReportPanel({
  tenantId,
  projectId,
}: {
  tenantId: string;
  projectId: string;
}) {
  const { session } = useAuth();
  const { t, i18n } = useTranslation("measurementReports");
  const [selected, setSelected] = useState<string>();
  const [saving, setSaving] = useState(false);
  const [saveState, setSaveState] = useState<"saved" | "saveFailed">();
  const saveIntent = useRef<MeasurementPeriodProjection | undefined>(undefined);
  const scope = [session?.user.id, session?.operator.id, tenantId, projectId];
  const enabled = Boolean(session);
  const preview = useQuery({
    queryKey: ["measurement-report-preview", ...scope],
    queryFn: () => getMeasurementReportPreview(tenantId, projectId),
    enabled,
    retry: false,
  });
  const history = useQuery({
    queryKey: ["measurement-reports", ...scope],
    queryFn: () => listMeasurementReports(tenantId, projectId),
    enabled,
    retry: false,
  });
  const detail = useQuery({
    queryKey: ["measurement-report", ...scope, selected],
    queryFn: () => getMeasurementReport(tenantId, projectId, selected!),
    enabled: enabled && Boolean(selected),
    retry: false,
  });
  const current = selected ? detail : preview;
  const report = current.data;
  const canWrite = session?.memberships.some(
    (member) =>
      member.tenant_id === tenantId &&
      ["tenant_admin", "member"].includes(member.role),
  );
  const at = (value: string, timezone = report?.report_timezone ?? "UTC") => {
    try {
      return new Intl.DateTimeFormat(i18n.language, {
        dateStyle: "medium",
        timeStyle: "short",
        timeZone: timezone,
      }).format(new Date(value));
    } catch {
      return value;
    }
  };
  const status = (value: string) =>
    t(knownStatuses.has(value) ? `status_${value}` : "status_other");
  async function save() {
    if (!preview.data || saving || !canWrite) return;
    saveIntent.current ??= preview.data;
    setSaving(true);
    setSaveState(undefined);
    try {
      const saved = await saveMeasurementReport(
        tenantId,
        projectId,
        saveIntent.current,
      );
      if (saved.project_id !== projectId)
        throw new Error("Report scope mismatch");
      saveIntent.current = undefined;
      setSaveState("saved");
      setSelected(saved.report_id);
      await history.refetch();
    } catch {
      setSaveState("saveFailed");
    } finally {
      setSaving(false);
    }
  }
  return (
    <section className="report-section" aria-label={t("title")}>
      <h2>{t("title")}</h2>
      <p>{t("description")}</p>
      <Button
        disabled={current.isFetching || saving}
        onClick={() => {
          void current.refetch();
          void history.refetch();
        }}
      >
        {t("refresh")}
      </Button>
      {selected && (
        <Button onClick={() => setSelected(undefined)}>{t("back")}</Button>
      )}
      {saveState && (
        <p role={saveState === "saveFailed" ? "alert" : "status"}>
          {t(saveState)}
        </p>
      )}
      {current.isPending ? (
        <p role="status">{t("loading")}</p>
      ) : current.isError ? (
        <p role="alert">{t("unavailable")}</p>
      ) : report?.project_id !== projectId ? (
        <p role="alert">{t("scopeError")}</p>
      ) : (
        report && (
          <>
            <h3>
              {report.kind === "measurement_period"
                ? t("version", { revision: report.revision })
                : t("preview")}
            </h3>
            <p>
              {report.kind === "measurement_period"
                ? t("immutable")
                : t("previewNote")}
            </p>
            {report.kind === "measurement_period" && report.correction_of && (
              <p>{t("correction", { id: report.correction_of })}</p>
            )}
            <p>
              {t("window", {
                start: at(report.report_window_start_at),
                end: at(report.report_window_end_at),
              })}{" "}
              · {report.report_timezone}
            </p>
            <p>{t("asOf", { time: at(report.evidence_as_of) })}</p>
            <h3>{t("aiTitle")}</h3>
            <p>
              {t("planned", { count: report.coverage.planned })} ·{" "}
              {t("live", { count: report.coverage.observed_live })} ·{" "}
              {t("analysis", {
                count: report.coverage.grounded_saved_analysis,
              })}
            </p>
            <ul>
              {Object.entries(report.coverage.counts).map(([key, count]) => (
                <li key={key}>
                  {status(key)}: {count}
                </li>
              ))}
            </ul>
            {!selected && canWrite && (
              <Button disabled={saving} onClick={() => void save()}>
                {t(saving ? "saving" : "save")}
              </Button>
            )}
            {!report.samples.length && <p>{t("empty")}</p>}
            {report.samples.map((sample) => (
              <Card
                className="report-panel"
                key={`${sample.target_id}/${sample.attempt_id ?? ""}`}
              >
                <h4>{reportMeasurementTitle(sample.comparison_key)}</h4>
                <p>
                  {t("original", { status: status(sample.original_status) })}
                </p>
                <p>{t("scheduled", { time: at(sample.scheduled_at) })}</p>
                <a
                  href={`/app/${encodeURIComponent(tenantId)}/${encodeURIComponent(projectId)}/measurement?tab=records&record=${encodeURIComponent(sample.plan_id)}`}
                >
                  {t("record")}
                </a>
                {sample.observation?.provenance && sample.attempt_id ? (
                  <ReportSupplementaryMeasurements
                    embedded
                    timezone={report.report_timezone}
                    items={[
                      {
                        ...sample,
                        attempt_id: sample.attempt_id,
                        observation: sample.observation,
                      },
                    ]}
                  />
                ) : sample.observation ? (
                  <>
                    <p>
                      {t("observed", {
                        time: at(sample.observation.observed_at),
                      })}
                    </p>
                    <h5>{t("answer")}</h5>
                    <p
                      style={{
                        whiteSpace: "pre-wrap",
                        overflowWrap: "anywhere",
                      }}
                    >
                      {sample.observation.raw_answer}
                    </p>
                    <h5>{t("citations")}</h5>
                    <ul>
                      {sample.observation.citations.map((url, index) => {
                        const href = safeOriginalPublicUrl(url);
                        return (
                          <li key={index}>
                            {href ? (
                              <a
                                href={href}
                                target="_blank"
                                rel="noopener noreferrer"
                              >
                                {url}
                              </a>
                            ) : (
                              url
                            )}
                          </li>
                        );
                      })}
                    </ul>
                  </>
                ) : (
                  <p>{t("noAnswer")}</p>
                )}
                {!sample.observation?.provenance && (
                  <details>
                    <summary>
                      {t("recordDetails", { ns: "reportAnalysis" })}
                    </summary>
                    <dl className="report-metadata">
                      <dt>{t("comparison", { ns: "reportAnalysis" })}</dt>
                      <dd>{sample.comparison_key}</dd>
                      <dt>{t("target", { ns: "reportAnalysis" })}</dt>
                      <dd>{sample.target_id}</dd>
                      {sample.attempt_id && (
                        <>
                          <dt>{t("attempt", { ns: "reportAnalysis" })}</dt>
                          <dd>{sample.attempt_id}</dd>
                        </>
                      )}
                      <dt>{t("plan", { ns: "reportAnalysis" })}</dt>
                      <dd>{sample.plan_id}</dd>
                    </dl>
                  </details>
                )}
              </Card>
            ))}
            <MeasurementReportSearchSection
              search={report.search}
              tenantId={tenantId}
              projectId={projectId}
              at={at}
            />
          </>
        )
      )}
      <h3>{t("history")}</h3>
      {history.isPending ? (
        <p>{t("loading")}</p>
      ) : history.isError ? (
        <p role="alert">{t("unavailable")}</p>
      ) : history.data.items.length ? (
        <ul>
          {history.data.items.map((item) => (
            <li key={item.report_id}>
              <Button onClick={() => setSelected(item.report_id)}>
                {at(item.report_window_start_at, item.report_timezone)} —{" "}
                {at(item.report_window_end_at, item.report_timezone)} ·{" "}
                {item.report_timezone} ·{" "}
                {t("view", { revision: item.revision })}
              </Button>
              {item.correction_of && (
                <span>{t("correction", { id: item.correction_of })}</span>
              )}
            </li>
          ))}
        </ul>
      ) : (
        <p>{t("noHistory")}</p>
      )}
    </section>
  );
}
