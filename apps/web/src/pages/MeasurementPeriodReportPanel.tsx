import { useRef, useState } from "react";
import { Button, Card, Field, Input } from "@fluentui/react-components";
import { useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { useAuth } from "../auth/AuthProvider";
import {
  getMeasurementReport,
  getMeasurementReportPreview,
  listMeasurementReports,
  saveMeasurementReport,
  type MeasurementPeriodProjection,
  type MeasurementPeriodReportSummary,
  type MeasurementPeriodSample,
  type MeasurementReportQuestionSet,
  type MeasurementReportWindow,
} from "../api/measurementReports";
import {
  RecommendationTrend,
  type RecommendationTrendSample,
} from "../components/RecommendationTrend";
import { ReportSupplementaryMeasurements } from "./ReportSupplementaryMeasurements";
import { safeOriginalPublicUrl } from "./PublicationLookupPanel";
import "../i18n/measurementReports";
import { reportMeasurementTitle } from "./reportMeasurementTitle";
import { MeasurementReportSearchSection } from "./MeasurementReportSearchSection";
import { decodeVerifiedRecommendations } from "../api/recommendations";
import "./MeasurementPeriodReportPanel.css";
import { useProjectQuery } from "../api/projects";
import {
  isMeasurementFocus,
  measurementFocusNames,
} from "../api/measurementFocus";
import { MeasurementFocusPanel } from "../components/MeasurementFocusPanel";
import { RecommendationFocusComparison } from "../components/RecommendationFocusComparison";
import { Link, useNavigate } from "react-router-dom";

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
  questionSet,
  selectedReportId,
  onSelectReport,
}: {
  tenantId: string;
  projectId: string;
  questionSet?: MeasurementReportQuestionSet;
  selectedReportId?: string;
  onSelectReport?: (reportId: string | undefined) => void;
}) {
  const { session } = useAuth();
  const navigate = useNavigate();
  const project = useProjectQuery(tenantId, projectId);
  const focusNames = measurementFocusNames(project.data?.settings);
  const { t, i18n } = useTranslation("measurementReports");
  const [localSelected, setLocalSelected] = useState<string>();
  const [questionFilter, setQuestionFilter] = useState("");
  const selected = onSelectReport ? selectedReportId : localSelected;
  const setSelected = (reportId: string | undefined) => {
    setQuestionFilter("");
    if (onSelectReport) onSelectReport(reportId);
    else setLocalSelected(reportId);
  };
  const [saving, setSaving] = useState(false);
  const [saveState, setSaveState] = useState<
    "saved" | "saveFailed" | "analysisScopeError"
  >();
  const [window, setWindow] = useState<MeasurementReportWindow>();
  const [start, setStart] = useState("");
  const [end, setEnd] = useState("");
  const [invalidWindow, setInvalidWindow] = useState(false);
  const saveIntent = useRef<
    | {
        projection: MeasurementPeriodProjection;
        analysisScope?: {
          questionTargetId?: string;
          question?: string;
        };
      }
    | undefined
  >(undefined);
  const scope = [
    session?.user.id,
    session?.operator.id,
    tenantId,
    projectId,
    questionSet?.question_set_id,
    questionSet?.question_set_version_id,
  ];
  const matchesScope = (
    value: Pick<MeasurementPeriodReportSummary, "project_id" | "question_set">,
  ) =>
    value.project_id === projectId &&
    (value.question_set?.question_set_id ?? null) ===
      (questionSet?.question_set_id ?? null) &&
    (value.question_set?.question_set_version_id ?? null) ===
      (questionSet?.question_set_version_id ?? null);
  const enabled = Boolean(session);
  const preview = useQuery({
    queryKey: ["measurement-report-preview", ...scope, window],
    queryFn: () =>
      getMeasurementReportPreview(tenantId, projectId, questionSet, window),
    enabled: enabled && !selected,
    retry: false,
  });
  const history = useQuery({
    queryKey: ["measurement-reports", ...scope],
    queryFn: () => listMeasurementReports(tenantId, projectId, questionSet),
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
  const orderedSamples = [...(report?.samples ?? [])].sort(
    (left, right) =>
      Date.parse(right.observation?.observed_at ?? right.scheduled_at) -
      Date.parse(left.observation?.observed_at ?? left.scheduled_at),
  );
  const questions = [
    ...new Set(orderedSamples.map((sample) => sample.conditions.question)),
  ];
  const activeQuestion = questionFilter;
  const visibleSamples = orderedSamples.filter(
    (sample) =>
      !activeQuestion || sample.conditions.question === activeQuestion,
  );
  const questionTargetId = activeQuestion
    ? visibleSamples[0]?.target_id
    : undefined;
  const hasOptimizationSamples = visibleSamples.some(
    (sample) => sample.question_binding?.purpose === "optimization",
  );
  const recordHref = (sample: MeasurementPeriodSample) => {
    const query = new URLSearchParams({
      tab: "records",
      record: sample.plan_id,
      target: sample.target_id,
    });
    const reference = sample.question_binding?.reference;
    if (reference) {
      query.set("set", reference.question_set_id);
      query.set("version", reference.question_set_version_id);
    }
    if (sample.attempt_id) query.set("attempt", sample.attempt_id);
    if (sample.observation?.provenance)
      query.set("analysisRevision", sample.observation.provenance.revision_id);
    return `/app/${encodeURIComponent(tenantId)}/${encodeURIComponent(projectId)}/measurement?${query}`;
  };
  const recommendations: RecommendationTrendSample[] = visibleSamples.map(
    (sample) => ({
      sampleId: sample.target_id,
      recordHref: recordHref(sample),
      observedAt: sample.observation?.observed_at ?? sample.scheduled_at,
      measurement: {
        ...sample.conditions,
        question_binding: sample.question_binding ?? undefined,
      },
      analysisRevisionId: sample.observation?.provenance?.revision_id,
      recommendations: sample.recommendations,
    }),
  );
  const hasAnswer = (sample: MeasurementPeriodProjection["samples"][number]) =>
    Boolean(sample.observation?.raw_answer.trim());
  const available = visibleSamples.filter(hasAnswer);
  const unavailable = visibleSamples.filter((sample) => !hasAnswer(sample));
  const canWrite = session?.memberships?.some(
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
  const reportChatHref = (
    reportId: string,
    selectedQuestionTargetId?: string,
  ) =>
    `/app/${encodeURIComponent(tenantId)}/${encodeURIComponent(projectId)}/chat?${new URLSearchParams({ measurementReport: reportId, ...(selectedQuestionTargetId ? { measurementQuestionTarget: selectedQuestionTargetId } : {}) })}`;
  async function save(analyze = false) {
    if (!preview.data || !matchesScope(preview.data) || saving || !canWrite)
      return;
    if (
      analyze &&
      !saveIntent.current?.analysisScope &&
      activeQuestion &&
      !questionTargetId
    ) {
      setSaveState("analysisScopeError");
      return;
    }
    saveIntent.current ??= {
      projection: preview.data,
    };
    if (analyze) {
      saveIntent.current.analysisScope ??= {
        questionTargetId,
        question: activeQuestion || undefined,
      };
    }
    const intent = saveIntent.current;
    const analysisScope = intent.analysisScope;
    setSaving(true);
    setSaveState(undefined);
    try {
      const saved = await saveMeasurementReport(
        tenantId,
        projectId,
        intent.projection,
      );
      if (!matchesScope(saved)) throw new Error("Report scope mismatch");
      if (
        analyze &&
        analysisScope?.question &&
        !saved.samples.some(
          (sample) =>
            sample.target_id === analysisScope.questionTargetId &&
            sample.conditions.question === analysisScope.question,
        )
      ) {
        saveIntent.current = undefined;
        setSaveState("analysisScopeError");
        return;
      }
      saveIntent.current = undefined;
      setSaveState("saved");
      if (analyze)
        navigate(
          reportChatHref(saved.report_id, analysisScope?.questionTargetId),
        );
      else {
        setSelected(saved.report_id);
        await history.refetch();
      }
    } catch {
      setSaveState("saveFailed");
    } finally {
      setSaving(false);
    }
  }
  const scopedReport = report && matchesScope(report) ? report : undefined;
  const reportActions = (
    <div className="measurement-report-actions">
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
        <Button onClick={() => setSelected(undefined)}>
          {t(questionSet ? "questionSetBack" : "back")}
        </Button>
      )}
      {scopedReport && !selected && canWrite && (
        <>
          <Button disabled={saving} onClick={() => void save()}>
            {t(saving ? "saving" : "save")}
          </Button>
          <Button disabled={saving} onClick={() => void save(true)}>
            {t("analyzeReport")}
          </Button>
        </>
      )}
      {scopedReport?.kind === "measurement_period" &&
        canWrite &&
        (!activeQuestion || questionTargetId) && (
          <Link to={reportChatHref(scopedReport.report_id, questionTargetId)}>
            {t("analyzeReport")}
          </Link>
        )}
      {saveState && (
        <p role={saveState === "saved" ? "status" : "alert"}>{t(saveState)}</p>
      )}
      {scopedReport && !hasOptimizationSamples && (
        <p>{t("analysisPurposeLimitation")}</p>
      )}
      {scopedReport && activeQuestion && !questionTargetId && (
        <p>{t("analysisScopeError")}</p>
      )}
    </div>
  );
  const reportDetails = (
    <details className="measurement-report-evidence measurement-report-details">
      <summary>{t("reportDetails")}</summary>
      {scopedReport && (
        <>
          <h3>
            {scopedReport.kind === "measurement_period"
              ? t("version", { revision: scopedReport.revision })
              : t(questionSet ? "questionSetPreview" : "preview")}
          </h3>
          <p>
            {scopedReport.kind === "measurement_period"
              ? t("immutable")
              : t("previewNote")}
          </p>
          {scopedReport.kind === "measurement_period" &&
            scopedReport.correction_of && (
              <p>{t("correction", { id: scopedReport.correction_of })}</p>
            )}
          <p>
            {t("window", {
              start: at(scopedReport.report_window_start_at),
              end: at(scopedReport.report_window_end_at),
            })}{" "}
            · {scopedReport.report_timezone}
          </p>
          <p>{t("asOf", { time: at(scopedReport.evidence_as_of) })}</p>
          <p>
            {t(activeQuestion ? "selectedQuestionScope" : "allQuestions")} ·{" "}
            {t("sampleTotal", { count: visibleSamples.length })} ·{" "}
            {t("answersAvailable", { count: available.length })} ·{" "}
            {t("answersUnavailable", { count: unavailable.length })}
          </p>
          <details>
            <summary>{t("originalResults")}</summary>
            <p>{t("wholeReportScope")}</p>
            <p>
              {t("planned", { count: scopedReport.coverage.planned })} ·{" "}
              {t("live", { count: scopedReport.coverage.observed_live })} ·{" "}
              {t("analysis", {
                count: scopedReport.coverage.grounded_saved_analysis,
              })}
            </p>
            <ul>
              {Object.entries(scopedReport.coverage.counts).map(
                ([key, count]) => (
                  <li key={key}>
                    {status(key)}: {count}
                  </li>
                ),
              )}
            </ul>
          </details>
        </>
      )}
      {history.data?.items.length ? (
        <Field label={t("historySelection")}>
          <select
            aria-label={t("historySelection")}
            className="measurement-report-select"
            value={selected ?? ""}
            onChange={(event) => setSelected(event.target.value || undefined)}
          >
            <option value="">
              {t(questionSet ? "questionSetPreview" : "preview")}
            </option>
            {history.data.items.filter(matchesScope).map((item) => (
              <option key={item.report_id} value={item.report_id}>
                {at(item.report_window_start_at, item.report_timezone)} —{" "}
                {at(item.report_window_end_at, item.report_timezone)} ·{" "}
                {t("view", { revision: item.revision })}
              </option>
            ))}
          </select>
        </Field>
      ) : null}
      {questionSet && (
        <details>
          <summary>{t("chooseWindow")}</summary>
          <form
            onSubmit={(event) => {
              event.preventDefault();
              const startAt = Date.parse(start);
              const endAt = Date.parse(end);
              if (
                !Number.isFinite(startAt) ||
                !Number.isFinite(endAt) ||
                startAt >= endAt
              ) {
                setInvalidWindow(true);
                return;
              }
              setInvalidWindow(false);
              setSelected(undefined);
              setSaveState(undefined);
              saveIntent.current = undefined;
              setWindow({
                start_at: new Date(startAt).toISOString(),
                end_at: new Date(endAt).toISOString(),
              });
            }}
          >
            <Field label={t("windowStart")}>
              <Input
                type="datetime-local"
                value={start}
                onChange={(_, data) => setStart(data.value)}
                required
              />
            </Field>
            <Field label={t("windowEnd")}>
              <Input
                type="datetime-local"
                value={end}
                onChange={(_, data) => setEnd(data.value)}
                required
              />
            </Field>
            <Button type="submit">{t("applyWindow")}</Button>
            <Button
              type="button"
              onClick={() => {
                setWindow(undefined);
                setStart("");
                setEnd("");
                setSelected(undefined);
                setSaveState(undefined);
                setInvalidWindow(false);
                saveIntent.current = undefined;
              }}
            >
              {t("allHistory")}
            </Button>
            {invalidWindow && <p role="alert">{t("invalidWindow")}</p>}
          </form>
        </details>
      )}
      <details className="measurement-report-evidence">
        <summary>{t("history")}</summary>
        {history.isPending ? (
          <p>{t("loading")}</p>
        ) : history.isError ? (
          <p role="alert">{t("unavailable")}</p>
        ) : history.data.items.some((item) => !matchesScope(item)) ? (
          <p role="alert">
            {t(questionSet ? "questionSetScopeError" : "scopeError")}
          </p>
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
      </details>
    </details>
  );
  return (
    <section
      className="report-section"
      aria-label={t(questionSet ? "questionSetTitle" : "title")}
    >
      <h2>{t(questionSet ? "questionSetTitle" : "title")}</h2>
      <p>{t(questionSet ? "questionSetDescription" : "description")}</p>
      <MeasurementFocusPanel
        tenantId={tenantId}
        projectId={projectId}
        canWrite={Boolean(canWrite)}
      />
      {current.isPending ? (
        <p role="status">{t("loading")}</p>
      ) : current.isError ? (
        <p role="alert">{t("unavailable")}</p>
      ) : report && !matchesScope(report) ? (
        <p role="alert">
          {t(questionSet ? "questionSetScopeError" : "scopeError")}
        </p>
      ) : (
        report && (
          <>
            {questions.length > 0 && (
              <Field label={t("questionFilter")} hint={t("latestFirst")}>
                <select
                  className="measurement-report-select"
                  aria-label={t("questionFilter")}
                  value={activeQuestion}
                  onChange={(event) => setQuestionFilter(event.target.value)}
                >
                  <option value="">{t("allQuestions")}</option>
                  {activeQuestion && !questions.includes(activeQuestion) && (
                    <option value={activeQuestion}>{activeQuestion}</option>
                  )}
                  {questions.map((question) => (
                    <option key={question} value={question}>
                      {question}
                    </option>
                  ))}
                </select>
              </Field>
            )}
            {recommendations.length > 0 && (
              <RecommendationTrend
                samples={recommendations}
                focusNames={focusNames}
              />
            )}
          </>
        )
      )}
      {reportActions}
      {reportDetails}
      {!current.isPending && !current.isError && scopedReport && (
        <>
          <h3>{t("aiTitle")}</h3>
          {!scopedReport.samples.length && <p>{t("empty")}</p>}
          {available.map((sample) => (
            <Card
              className="report-panel measurement-report-sample"
              key={`${sample.target_id}/${sample.attempt_id ?? ""}`}
            >
              <h4>{sample.conditions.question}</h4>
              <p className="report-muted">
                {reportMeasurementTitle(sample.comparison_key)} ·{" "}
                {at(sample.observation?.observed_at ?? sample.scheduled_at)}
              </p>
              <SampleRecommendations sample={sample} focusNames={focusNames} />
              <a href={recordHref(sample)}>{t("record")}</a>
              <details className="measurement-report-evidence">
                <summary>{t("answerEvidence")}</summary>
                {sample.observation?.provenance && sample.attempt_id ? (
                  <ReportSupplementaryMeasurements
                    embedded
                    timezone={scopedReport.report_timezone}
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
                ) : null}
                <details>
                  <summary>{t("originalReceipt")}</summary>
                  <p>
                    {t("original", {
                      status: status(sample.original_status),
                    })}
                  </p>
                  <p>{t("scheduled", { time: at(sample.scheduled_at) })}</p>
                </details>
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
              </details>
            </Card>
          ))}
          {unavailable.length > 0 && (
            <details className="measurement-report-evidence">
              <summary>
                {t("unavailableDetails", { count: unavailable.length })}
              </summary>
              <ul className="measurement-report-unavailable">
                {unavailable.map((sample) => (
                  <li key={`${sample.target_id}/${sample.attempt_id ?? ""}`}>
                    <strong>{sample.conditions.question}</strong>
                    <p>
                      {reportMeasurementTitle(sample.comparison_key)} ·{" "}
                      {at(sample.scheduled_at)} ·{" "}
                      {status(sample.original_status)}
                    </p>
                    <details>
                      <summary>{t("originalReceipt")}</summary>
                      <p>
                        {t("original", {
                          status: status(sample.original_status),
                        })}
                      </p>
                      {sample.observation && (
                        <>
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
                          <pre
                            style={{
                              whiteSpace: "pre-wrap",
                              overflowWrap: "anywhere",
                            }}
                          >
                            {JSON.stringify(sample.observation, null, 2)}
                          </pre>
                        </>
                      )}
                    </details>
                    <a href={recordHref(sample)}>{t("record")}</a>
                  </li>
                ))}
              </ul>
            </details>
          )}
          <MeasurementReportSearchSection
            search={scopedReport.search}
            tenantId={tenantId}
            projectId={projectId}
            at={at}
          />
        </>
      )}
    </section>
  );
}

/** Display persisted AI extraction only; never infer rankings from answer text. */
function SampleRecommendations({
  sample,
  focusNames,
}: {
  sample: MeasurementPeriodSample;
  focusNames: readonly string[];
}) {
  const { t } = useTranslation("measurementReports");
  const items = decodeVerifiedRecommendations(sample.recommendations);
  if (items === null) return <p>{t("recommendationsPending")}</p>;
  if (!items.length) return <p>{t("recommendationsEmpty")}</p>;
  const focused = items.filter((item) =>
    isMeasurementFocus(item.name, focusNames),
  );
  const table = (
    <table className="report-table">
      <caption>{t("recommendedEntities")}</caption>
      <thead>
        <tr>
          <th scope="col">{t("entity")}</th>
          <th scope="col">{t("rank")}</th>
          <th scope="col">{t("reason")}</th>
        </tr>
      </thead>
      <tbody>
        {[...items]
          .sort((a, b) => a.mention_order - b.mention_order)
          .map((item, index) => (
            <tr
              key={`${item.kind}/${item.name}/${index}`}
              className={
                isMeasurementFocus(item.name, focusNames)
                  ? "measurement-report-focus-row"
                  : undefined
              }
            >
              <th scope="row">{item.name}</th>
              <td>
                {item.rank === null
                  ? t("unranked")
                  : t("rankValue", { rank: item.rank })}
              </td>
              <td>
                {item.reason || t("reasonUnavailable")}
                <details>
                  <summary>{t("recommendationEvidence")}</summary>
                  <blockquote>{item.evidence_quote}</blockquote>
                  {item.citation_urls.map((url, linkIndex) => {
                    const href = safeOriginalPublicUrl(url);
                    return href ? (
                      <a
                        key={linkIndex}
                        href={href}
                        target="_blank"
                        rel="noopener noreferrer"
                      >
                        {url}
                      </a>
                    ) : null;
                  })}
                </details>
              </td>
            </tr>
          ))}
      </tbody>
    </table>
  );
  return (
    <div className="measurement-report-recommendations">
      {focusNames.length > 0 && focused.length === 0 && (
        <p className="measurement-report-focus">
          <strong>{t("currentFocus", { name: focusNames.join("、") })}</strong>
          {" · "}
          {t("focusNotFound")}
        </p>
      )}
      <RecommendationFocusComparison items={items} focusNames={focusNames} />
      {focusNames.length > 0 ? (
        <details className="measurement-report-evidence">
          <summary>
            {t("allRecommendedEntities", { count: items.length })}
          </summary>
          {table}
        </details>
      ) : (
        table
      )}
    </div>
  );
}
