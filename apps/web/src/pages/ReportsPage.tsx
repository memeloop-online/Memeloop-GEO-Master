import { useLayoutEffect, useRef, useState } from "react";
import {
  Badge,
  Button,
  Card,
  MessageBar,
  MessageBarBody,
} from "@fluentui/react-components";
import { ArrowSyncRegular } from "@fluentui/react-icons";
import { Link, useParams } from "react-router-dom";
import { useTranslation } from "react-i18next";
import {
  useCurrentReportCycleQuery,
  useReportEvidenceQuery,
  useReportPreviewQuery,
  useReportQuery,
  useReportsQuery,
  type ReportCoverage,
  type ReportEvidenceReference,
  type ReportProjection,
  type ReportSnapshot,
} from "../api/reports";
import { ApiError } from "../api/client";
import { EmptyState, ErrorState, LoadingState } from "../components/AsyncState";
import { downloadReportCsv } from "./reportsCsv";
import { ReportSupplementaryMeasurements } from "./ReportSupplementaryMeasurements";
import { MeasurementPeriodReportPanel } from "./MeasurementPeriodReportPanel";
import { reportMeasurementTitle } from "./reportMeasurementTitle";
import { reportAnalysisText } from "../i18n/reportAnalysis";
import i18n, { formatUiNumber } from "../i18n";
import { cycleReportsText } from "../i18n/cycleReports";
import "./ReportsPage.css";

const coverageKeys = new Set([
  "planned",
  "blocked",
  "deferred",
  "cancelled",
  "not_applicable",
  "published",
  "verified",
  "unknown",
  "failed",
  "missing",
  "pending",
  "unmaterialized",
  "observed",
  "refused",
  "not_mentioned",
]);

function dateTime(value: string | null, timezone: string): string {
  if (!value) return cycleReportsText("notRecorded");
  const parsed = Date.parse(value);
  if (Number.isNaN(parsed)) return value;
  const options: Intl.DateTimeFormatOptions = {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  };
  try {
    return new Intl.DateTimeFormat(i18n.resolvedLanguage ?? i18n.language, {
      ...options,
      timeZone: timezone,
    }).format(parsed);
  } catch (error) {
    if (!(error instanceof RangeError)) throw error;
    return cycleReportsText("timezoneFallback", {
      time: new Intl.DateTimeFormat(i18n.resolvedLanguage ?? i18n.language, {
        ...options,
        timeZone: "UTC",
      }).format(parsed),
    });
  }
}

function coverageName(key: string) {
  return cycleReportsText(`coverage.${coverageKeys.has(key) ? key : "other"}`);
}

function CoveragePanel({
  title,
  coverage,
  note,
}: {
  title: string;
  coverage: ReportCoverage;
  note: string;
}) {
  const { t: ct } = useTranslation("cycleReports");
  const available = coverage.availability === "available";
  return (
    <Card className="report-panel">
      <h2>{title}</h2>
      <p>{note}</p>
      <p>
        {ct("coverageSummary", {
          availability: ct(
            coverage.availability === "available"
              ? "available"
              : coverage.availability === "unsealed"
                ? "unsealed"
                : "unavailable",
          ),
          expected:
            coverage.expected_count === null
              ? ct("unknown")
              : formatUiNumber(coverage.expected_count),
          observed: formatUiNumber(coverage.observed_count),
        })}
      </p>
      {!available && (
        <MessageBar intent="warning">
          <MessageBarBody>
            {coverage.reason || ct("planUnavailable")}
          </MessageBarBody>
        </MessageBar>
      )}
      {Object.keys(coverage.counts).length > 0 ? (
        <table className="report-table">
          <thead>
            <tr>
              <th scope="col">{ct("state")}</th>
              <th scope="col">{ct("quantity")}</th>
            </tr>
          </thead>
          <tbody>
            {Object.entries(coverage.counts).map(([key, count]) => (
              <tr key={key}>
                <th scope="row">{coverageName(key)}</th>
                <td>{formatUiNumber(count)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : (
        <p className="report-muted">{ct("noCounts")}</p>
      )}
    </Card>
  );
}

function EvidenceItem({
  item,
  timezone,
}: {
  item: ReportEvidenceReference;
  timezone: string;
}) {
  const { t: ct } = useTranslation("cycleReports");
  return (
    <li id={`evidence-${encodeURIComponent(item.evidence_id)}`}>
      <article className="report-evidence">
        <h3>{item.summary}</h3>
        <dl>
          <dt>{ct("evidenceType")}</dt>
          <dd>{item.kind}</dd>
          <dt>{ct("resourceId")}</dt>
          <dd>{item.resource_id}</dd>
          <dt>{ct("resourceVersion")}</dt>
          <dd>{item.resource_version ?? ct("notRecorded")}</dd>
          <dt>{ct("occurred")}</dt>
          <dd>{dateTime(item.occurred_at, timezone)}</dd>
          <dt>{ct("received")}</dt>
          <dd>{dateTime(item.received_at, timezone)}</dd>
          <dt>{ct("evidenceId")}</dt>
          <dd>{item.evidence_id}</dd>
        </dl>
      </article>
    </li>
  );
}

function SnapshotDetail({
  snapshot,
  savedSnapshot,
  evidence,
  evidenceError,
  evidenceLoading,
  onEvidenceRetry,
  onRefresh,
  refreshing,
}: {
  snapshot: ReportProjection;
  savedSnapshot?: ReportSnapshot;
  evidence: ReportEvidenceReference[];
  evidenceError: Error | null;
  evidenceLoading: boolean;
  onEvidenceRetry: () => void;
  onRefresh: () => void;
  refreshing: boolean;
}) {
  const { t: ct } = useTranslation("cycleReports");
  const evidenceIds = new Set(evidence.map((item) => item.evidence_id));
  const Container = savedSnapshot ? "div" : "section";
  const [pdfExporting, setPdfExporting] = useState(false);
  const [pdfError, setPdfError] = useState(false);
  const pdfController = useRef<AbortController | null>(null);

  useLayoutEffect(() => {
    return () => {
      pdfController.current?.abort();
      pdfController.current = null;
    };
  }, [savedSnapshot?.report_id, savedSnapshot?.input_hash]);

  async function exportPdf() {
    if (!savedSnapshot || pdfController.current) return;
    const controller = new AbortController();
    pdfController.current = controller;
    setPdfError(false);
    setPdfExporting(true);
    try {
      const { downloadReportPdf } = await import("./reportsPdf");
      if (controller.signal.aborted) return;
      await downloadReportPdf(savedSnapshot, { signal: controller.signal });
    } catch {
      if (!controller.signal.aborted) setPdfError(true);
    } finally {
      if (pdfController.current === controller) {
        pdfController.current = null;
        if (!controller.signal.aborted) setPdfExporting(false);
      }
    }
  }

  return (
    <Container
      className={
        savedSnapshot
          ? "workbench-page reports-page"
          : "reports-page report-preview"
      }
      aria-label={savedSnapshot ? undefined : ct("previewAria")}
    >
      {savedSnapshot ? (
        <>
          <section className="page-hero">
            <div>
              <p className="eyebrow">{ct("formal")}</p>
              <h1>
                {ct("weeklyTitle", {
                  time: dateTime(
                    snapshot.report_window_start_at,
                    snapshot.report_timezone,
                  ),
                })}
              </h1>
              <p>
                {dateTime(
                  snapshot.report_window_start_at,
                  snapshot.report_timezone,
                )}{" "}
                —{" "}
                {dateTime(
                  snapshot.report_window_end_at,
                  snapshot.report_timezone,
                )}
                {" · "}
                {ct("timezone", { timezone: snapshot.report_timezone })}
              </p>
            </div>
            <div className="report-actions">
              <Button onClick={() => downloadReportCsv(savedSnapshot)}>
                {ct("downloadCsv")}
              </Button>
              <Button disabled={pdfExporting} onClick={() => void exportPdf()}>
                {ct(pdfExporting ? "generatingPdf" : "downloadPdf")}
              </Button>
              <Button
                icon={<ArrowSyncRegular />}
                disabled={refreshing}
                onClick={onRefresh}
              >
                {ct("refreshSnapshot")}
              </Button>
            </div>
          </section>
          {pdfError && (
            <MessageBar intent="error">
              <MessageBarBody>{ct("pdfError")}</MessageBarBody>
            </MessageBar>
          )}
          <Link to="../reports">{ct("back")}</Link>
          <MessageBar
            intent={snapshot.status === "partial" ? "warning" : "info"}
          >
            <MessageBarBody>
              {ct("immutableNotice", {
                status: ct(
                  snapshot.status === "partial" ? "partial" : "complete",
                ),
                cutoff: dateTime(snapshot.cutoff_at, snapshot.report_timezone),
                generated: dateTime(
                  snapshot.generated_at,
                  snapshot.report_timezone,
                ),
              })}
            </MessageBarBody>
          </MessageBar>
        </>
      ) : (
        <>
          <div className="report-list-heading">
            <h2>{ct("previewTitle")}</h2>
            <Button
              icon={<ArrowSyncRegular />}
              disabled={refreshing}
              onClick={onRefresh}
            >
              {ct("refreshPreview")}
            </Button>
          </div>
          <MessageBar intent="warning">
            <MessageBarBody>
              {ct("previewNotice")}{" "}
              {ct(snapshot.status === "partial" ? "partial" : "complete")}
            </MessageBarBody>
          </MessageBar>
          <dl className="report-metadata">
            <dt>{ct("generated")}</dt>
            <dd>{dateTime(snapshot.generated_at, snapshot.report_timezone)}</dd>
            <dt>{ct("evidenceAsOf")}</dt>
            <dd>
              {dateTime(snapshot.evidence_as_of, snapshot.report_timezone)}
            </dd>
            <dt>{ct("cutoff")}</dt>
            <dd>{dateTime(snapshot.cutoff_at, snapshot.report_timezone)}</dd>
          </dl>
        </>
      )}
      <Card className="report-panel">
        <h2>{ct(savedSnapshot ? "reportScope" : "previewScope")}</h2>
        <p>
          {dateTime(snapshot.report_window_start_at, snapshot.report_timezone)}{" "}
          — {dateTime(snapshot.report_window_end_at, snapshot.report_timezone)}
          {" · "}
          {ct("timezone", { timezone: snapshot.report_timezone })}
        </p>
        <dl className="report-metadata">
          {savedSnapshot && (
            <>
              <dt>{ct("reportId")}</dt>
              <dd>{savedSnapshot.report_id}</dd>
            </>
          )}
          <dt>{ct("cycleId")}</dt>
          <dd>{snapshot.cycle_id}</dd>
          {savedSnapshot && (
            <>
              <dt>{ct("revision")}</dt>
              <dd>{formatUiNumber(savedSnapshot.revision)}</dd>
              <dt>{ct("correctionOf")}</dt>
              <dd>{savedSnapshot.correction_of ?? ct("none")}</dd>
            </>
          )}
          {savedSnapshot && (
            <>
              <dt>{ct("evidenceAsOf")}</dt>
              <dd>
                {dateTime(snapshot.evidence_as_of, snapshot.report_timezone)}
              </dd>
            </>
          )}
        </dl>
        <h3>{ct("planVersions")}</h3>
        {snapshot.input_manifest_versions.length ? (
          <ul>
            {snapshot.input_manifest_versions.map((item) => (
              <li key={`${item.kind}/${item.manifest_id}/${item.revision}`}>
                {ct("manifestLine", {
                  kind: ct(`manifest.${item.kind}`),
                  id: item.manifest_id,
                  revision: formatUiNumber(item.revision),
                  sealed: ct(item.sealed ? "sealed" : "unsealed"),
                  expected:
                    item.expected_count === null
                      ? ct("unknown")
                      : formatUiNumber(item.expected_count),
                })}
              </li>
            ))}
          </ul>
        ) : (
          <p>{ct("noPlanVersions")}</p>
        )}
      </Card>
      <section className="report-coverage" aria-label={ct("coverageAria")}>
        <CoveragePanel
          title={ct("documentsTitle")}
          coverage={snapshot.documents}
          note={ct("documentsNote")}
        />
        <CoveragePanel
          title={ct("publicationsTitle")}
          coverage={snapshot.publications}
          note={ct("publicationsNote")}
        />
        <CoveragePanel
          title={ct("measurementsTitle")}
          coverage={snapshot.measurements}
          note={ct("measurementsNote")}
        />
      </section>
      <section className="report-section" aria-label={ct("groupsAria")}>
        <h2>{ct("groupsTitle")}</h2>
        <div className="report-coverage">
          <Card className="report-panel">
            <h3>{ct("publicationPlatforms")}</h3>
            {snapshot.publication_groups.length ? (
              snapshot.publication_groups.map((group) => (
                <div key={group.platform_id} className="report-group">
                  <h4>{group.platform_id}</h4>
                  <p>
                    {ct("groupSummary", {
                      availability:
                        group.coverage.availability === "available"
                          ? ct("available")
                          : group.coverage.reason || ct("unavailable"),
                      expected:
                        group.coverage.expected_count === null
                          ? ct("unknown")
                          : formatUiNumber(group.coverage.expected_count),
                      observed: formatUiNumber(group.coverage.observed_count),
                    })}
                  </p>
                  <p>
                    {Object.entries(group.coverage.counts)
                      .map(([key, count]) =>
                        ct("statusCount", {
                          status: coverageName(key),
                          value: formatUiNumber(count),
                        }),
                      )
                      .join(" · ") || ct("noGroupCounts")}
                  </p>
                  {group.coverage.reason && (
                    <p className="report-muted">{group.coverage.reason}</p>
                  )}
                </div>
              ))
            ) : (
              <p>{ct("noPublications")}</p>
            )}
          </Card>
          <Card className="report-panel">
            <h3>{ct("measurementProtocols")}</h3>
            {snapshot.measurement_groups.length ? (
              snapshot.measurement_groups.map((group) => (
                <div key={group.comparison_key} className="report-group">
                  <h4>{reportMeasurementTitle(group.comparison_key)}</h4>
                  <details>
                    <summary>{reportAnalysisText("recordDetails")}</summary>
                    <p>{group.comparison_key}</p>
                  </details>
                  <p>
                    {ct("purpose", {
                      purpose: ct(
                        group.purpose === "optimization"
                          ? "optimization"
                          : group.purpose === "frozen_evaluation"
                            ? "frozenEvaluation"
                            : "unclassified",
                      ),
                    })}
                  </p>
                  <p>
                    {ct("groupSummary", {
                      availability:
                        group.coverage.availability === "available"
                          ? ct("available")
                          : group.coverage.reason || ct("unavailable"),
                      expected:
                        group.coverage.expected_count === null
                          ? ct("unknown")
                          : formatUiNumber(group.coverage.expected_count),
                      observed: formatUiNumber(group.coverage.observed_count),
                    })}
                  </p>
                  <p>
                    {Object.entries(group.coverage.counts)
                      .map(([key, count]) =>
                        ct("statusCount", {
                          status: coverageName(key),
                          value: formatUiNumber(count),
                        }),
                      )
                      .join(" · ") || ct("noGroupCounts")}
                  </p>
                  {group.coverage.reason && (
                    <p className="report-muted">{group.coverage.reason}</p>
                  )}
                </div>
              ))
            ) : (
              <p>{ct("noMeasurements")}</p>
            )}
          </Card>
        </div>
        <p>{ct("groupNote")}</p>
      </section>
      {!!snapshot.supplementary_measurements?.length && (
        <ReportSupplementaryMeasurements
          items={snapshot.supplementary_measurements}
          timezone={snapshot.report_timezone}
        />
      )}
      <section className="report-section" aria-label={ct("findingsAria")}>
        <h2>{ct("findingsTitle")}</h2>
        {snapshot.findings.length ? (
          <ul className="report-findings">
            {snapshot.findings.map((finding) => (
              <li key={finding.finding_id}>
                <Card className="report-panel">
                  <h3>{finding.summary}</h3>
                  <p>{ct("type", { kind: finding.kind })}</p>
                  {finding.insufficient_reason && (
                    <p>
                      {ct("insufficient", {
                        reason: finding.insufficient_reason,
                      })}
                    </p>
                  )}
                  {finding.evidence_ids.length ? (
                    <ul>
                      {finding.evidence_ids.map((id) => (
                        <li key={id}>
                          {evidenceIds.has(id) ? (
                            <a href={`#evidence-${encodeURIComponent(id)}`}>
                              {ct(
                                savedSnapshot
                                  ? "savedEvidence"
                                  : "previewEvidence",
                                { id },
                              )}
                            </a>
                          ) : evidenceLoading || evidenceError ? (
                            <>{ct("evidenceUnavailable", { id })}</>
                          ) : (
                            <>{ct("evidenceMissing", { id })}</>
                          )}
                        </li>
                      ))}
                    </ul>
                  ) : (
                    <p>{ct("noFindingSource")}</p>
                  )}
                </Card>
              </li>
            ))}
          </ul>
        ) : (
          <p>{ct(savedSnapshot ? "noSavedFindings" : "noPreviewFindings")}</p>
        )}
      </section>
      <section className="report-section" aria-label={ct("evidenceTitle")}>
        <h2>{ct("evidenceTitle")}</h2>
        {evidenceLoading && (
          <LoadingState label={ct("loadingEvidence")} compact />
        )}
        {evidenceError && (
          <ErrorState
            title={ct("evidenceError")}
            detail={ct("errorHint")}
            onRetry={onEvidenceRetry}
          />
        )}
        {!evidenceLoading && !evidenceError && evidence.length ? (
          <ul className="report-evidence-list">
            {evidence.map((item) => (
              <EvidenceItem
                key={item.evidence_id}
                item={item}
                timezone={snapshot.report_timezone}
              />
            ))}
          </ul>
        ) : !evidenceLoading && !evidenceError ? (
          <p>{ct(savedSnapshot ? "noSavedEvidence" : "noPreviewEvidence")}</p>
        ) : null}
      </section>
      {savedSnapshot && (
        <MessageBar intent="info">
          <MessageBarBody>{ct("exportNote")}</MessageBarBody>
        </MessageBar>
      )}
    </Container>
  );
}

function ReportListPage({
  tenantId,
  projectId,
}: {
  tenantId: string;
  projectId: string;
}) {
  const { t } = useTranslation("measurementReports");
  const { t: ct } = useTranslation("cycleReports");
  const query = useReportsQuery(tenantId, projectId);
  const cycle = useCurrentReportCycleQuery(tenantId, projectId);
  const preview = useReportPreviewQuery(
    tenantId,
    projectId,
    cycle.data?.cycle_id,
  );
  return (
    <div className="workbench-page reports-page">
      <section className="page-hero">
        <div>
          <h1>{t("pageTitle")}</h1>
          <p>{t("pageDescription")}</p>
        </div>
      </section>
      <MeasurementPeriodReportPanel
        key={`${tenantId}/${projectId}`}
        tenantId={tenantId}
        projectId={projectId}
        embedded
      />
      <details className="measurement-report-evidence">
        <summary>{t("cycleReports")}</summary>
        <Button
          icon={<ArrowSyncRegular />}
          disabled={query.isFetching}
          onClick={() => void query.refetch()}
        >
          {ct("refreshList")}
        </Button>
        {cycle.isPending ? (
          <LoadingState label={ct("loadingCycle")} compact />
        ) : cycle.isError ? (
          <ErrorState
            title={
              cycle.error instanceof ApiError && cycle.error.status === 403
                ? ct("permission")
                : ct("cycleError")
            }
            detail={ct("errorHint")}
            onRetry={() => void cycle.refetch()}
          />
        ) : !cycle.data ? (
          <EmptyState
            title={ct("noCycle")}
            detail={ct("noCycleHint")}
            action={
              <Button
                icon={<ArrowSyncRegular />}
                onClick={() => void cycle.refetch()}
              >
                {ct("checkCycle")}
              </Button>
            }
          />
        ) : preview.isPending ? (
          <LoadingState label={ct("loadingPreview")} compact />
        ) : preview.isError ? (
          <ErrorState
            title={
              preview.error instanceof ApiError && preview.error.status === 403
                ? ct("permission")
                : ct("previewError")
            }
            detail={ct("errorHint")}
            onRetry={() => void preview.refetch()}
          />
        ) : preview.data.project_id !== projectId ||
          preview.data.cycle_id !== cycle.data.cycle_id ||
          preview.data.kind !== "preview" ? (
          <ErrorState
            title={ct("previewScopeError")}
            detail={ct("refreshCycleHint")}
            onRetry={() => void cycle.refetch()}
          />
        ) : (
          <SnapshotDetail
            snapshot={preview.data}
            evidence={preview.data.evidence}
            evidenceError={null}
            evidenceLoading={false}
            onEvidenceRetry={() => {}}
            refreshing={preview.isFetching}
            onRefresh={() => void preview.refetch()}
          />
        )}
        <section aria-label={ct("savedTitle")}>
          <h2>{ct("savedTitle")}</h2>
          {query.isPending ? (
            <LoadingState label={ct("loadingReports")} compact />
          ) : query.isError ? (
            <ErrorState
              title={
                query.error instanceof ApiError && query.error.status === 403
                  ? ct("permission")
                  : ct("reportError")
              }
              detail={ct("errorHint")}
              onRetry={() => void query.refetch()}
            />
          ) : query.data.items.length === 0 ? (
            <EmptyState title={ct("noReports")} detail={ct("noReportsHint")} />
          ) : (
            <ul className="report-list">
              {query.data.items.map((snapshot) => (
                <li key={snapshot.report_id}>
                  <Card className="report-panel">
                    <div className="report-list-heading">
                      <h2>
                        <Link to={encodeURIComponent(snapshot.report_id)}>
                          {dateTime(
                            snapshot.report_window_start_at,
                            snapshot.report_timezone,
                          )}{" "}
                          —{" "}
                          {dateTime(
                            snapshot.report_window_end_at,
                            snapshot.report_timezone,
                          )}
                        </Link>
                      </h2>
                      <Badge
                        appearance="tint"
                        color={
                          snapshot.status === "partial" ? "warning" : "success"
                        }
                      >
                        {ct(
                          snapshot.status === "partial"
                            ? "partial"
                            : "complete",
                        )}
                      </Badge>
                    </div>
                    <p>
                      {ct("listMetadata", {
                        cycle: snapshot.cycle_id,
                        revision: formatUiNumber(snapshot.revision),
                        cutoff: dateTime(
                          snapshot.cutoff_at,
                          snapshot.report_timezone,
                        ),
                      })}
                    </p>
                    {snapshot.correction_of && (
                      <p>{ct("correction", { id: snapshot.correction_of })}</p>
                    )}
                  </Card>
                </li>
              ))}
            </ul>
          )}
        </section>
      </details>
    </div>
  );
}

function ReportDetailPage({
  tenantId,
  projectId,
  reportId,
}: {
  tenantId: string;
  projectId: string;
  reportId: string;
}) {
  const { t: ct } = useTranslation("cycleReports");
  const query = useReportQuery(tenantId, projectId, reportId);
  const evidence = useReportEvidenceQuery(tenantId, projectId, reportId);
  if (query.isPending) return <LoadingState label={ct("loadingReport")} />;
  if (query.isError) {
    return (
      <ErrorState
        title={
          query.error instanceof ApiError && query.error.status === 403
            ? ct("permission")
            : query.error instanceof ApiError && query.error.status === 404
              ? ct("notFound")
              : ct("reportError")
        }
        detail={ct("errorHint")}
        onRetry={() => void query.refetch()}
      />
    );
  }
  if (query.data.project_id !== projectId) {
    return (
      <ErrorState
        title={ct("reportScopeError")}
        detail={ct("reportScopeHint")}
      />
    );
  }
  return (
    <SnapshotDetail
      snapshot={query.data}
      savedSnapshot={query.data}
      evidence={evidence.data?.items ?? []}
      evidenceError={evidence.isError ? evidence.error : null}
      evidenceLoading={evidence.isPending}
      onEvidenceRetry={() => void evidence.refetch()}
      refreshing={query.isFetching}
      onRefresh={() => void query.refetch()}
    />
  );
}

export function ReportsPage() {
  const { t: ct } = useTranslation("cycleReports");
  const { tenantId, projectId, id } = useParams();
  if (!tenantId || !projectId) {
    return <ErrorState title={ct("missingProject")} />;
  }
  return id ? (
    <ReportDetailPage
      key={`${tenantId}/${projectId}/${id}`}
      tenantId={tenantId}
      projectId={projectId}
      reportId={id}
    />
  ) : (
    <ReportListPage
      key={`${tenantId}/${projectId}`}
      tenantId={tenantId}
      projectId={projectId}
    />
  );
}
