import { useState } from "react";
import {
  Badge,
  Button,
  Card,
  MessageBar,
  MessageBarBody,
  Select,
  Spinner,
} from "@fluentui/react-components";
import {
  ArrowLeftRegular,
  ArrowSyncRegular,
  OpenRegular,
} from "@fluentui/react-icons";
import { Link, useNavigate, useParams } from "react-router-dom";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import "../i18n/sourceDetail";
import {
  type ImportJob,
  useRetryImportJobMutation,
  useSourceQuery,
} from "../api/knowledge";
import { useAuth } from "../auth/AuthProvider";
import { membershipForTenant } from "../auth/types";
import { ErrorState, EmptyState, LoadingState } from "../components/AsyncState";
import { KnowledgeLocator } from "../components/KnowledgeLocator";
import { CsvEvidenceTable } from "../components/CsvEvidenceTable";
import { OfficeEvidenceTable } from "../components/OfficeEvidenceTable";
import { SourceTextRevisionPanel } from "../components/SourceTextRevisionPanel";
import { StatusPill, type StatusKind } from "../components/StatusPill";

function statusKind(value: string | null | undefined): StatusKind {
  switch (value) {
    case "succeeded":
    case "complete":
    case "completed":
    case "available":
    case "ready":
      return "complete";
    case "failed":
    case "blocked":
    case "removed":
    case "conflicted":
      return "blocked";
    case "queued":
      return "queued";
    case "running":
    case "active":
    case "processing":
      return "running";
    default:
      return "uncertain";
  }
}

const pdfFailureReasons: Record<string, string> = {
  ocr_required: "ocr_required",
  empty_text: "empty_text",
  parse_failed: "parse_failed",
  page_limit: "page_limit",
  invalid_pdf: "invalid_pdf",
  encrypted_pdf: "encrypted_pdf",
};

const officeFailureReasons: Record<string, string> = {
  invalid_docx: "invalid_docx",
  invalid_xlsx: "invalid_xlsx",
  encrypted_office: "encrypted_office",
  parse_failed: "office_parse_failed",
  unit_limit: "unit_limit",
  unsupported_content: "unsupported_content",
  empty_text: "office_empty_text",
};

type SourceFormat = "pdf" | "docx" | "xlsx" | "other";

function safeFailure(
  error: NonNullable<ImportJob["errors"]>[number],
  format: SourceFormat,
  t: TFunction,
) {
  if (format === "docx" || format === "xlsx") {
    const id =
      Number.isInteger(error.unit_id) && (error.unit_id ?? -1) >= 0
        ? t("sourceDetail.failure.unit", { unit: (error.unit_id ?? 0) + 1 })
        : "";
    const reason = officeFailureReasons[error.code ?? ""];
    return `${id}${t(`sourceDetail.failure.${reason ?? "officeFallback"}`)}`;
  }
  if (format !== "pdf") return t("sourceDetail.failure.fallback");
  const unit = typeof error.unit === "string" ? error.unit : "";
  const unitPage = /^page[:_ -]?([1-9]\d{0,5})$/.exec(unit);
  const page =
    typeof error.page === "number" &&
    Number.isInteger(error.page) &&
    error.page > 0 &&
    error.page <= 999999
      ? error.page
      : unitPage
        ? Number(unitPage[1])
        : null;
  const position = page ? t("sourceDetail.failure.page", { page }) : "";
  const reason = pdfFailureReasons[error.code ?? ""];
  return `${position}${t(`sourceDetail.failure.${reason ?? "fallback"}`)}`;
}

function importStatusLabel(value: string | null | undefined, t: TFunction) {
  const labels: Record<string, string> = {
    queued: "queued",
    running: "running",
    partial: "partial",
    succeeded: "succeeded",
    failed: "failed",
    cancelled: "cancelled",
  };
  const known = labels[value ?? ""];
  return known
    ? t(`sourceDetail.status.${known}`)
    : (value ?? t("sourceDetail.status.default"));
}

export function SourceDetailPage() {
  const { t } = useTranslation();
  const { tenantId, projectId, id } = useParams();
  const { session } = useAuth();
  const membership = membershipForTenant(session, tenantId ?? "");
  const canRetry =
    membership?.role === "tenant_admin" || membership?.role === "member";
  const navigate = useNavigate();
  const sourceQuery = useSourceQuery(tenantId, projectId, id);
  const retryJob = useRetryImportJobMutation(tenantId, projectId);
  const knowledgePath =
    tenantId && projectId
      ? `/app/${encodeURIComponent(tenantId)}/${encodeURIComponent(projectId)}/knowledge`
      : "../knowledge";
  const [selectedChunkId, setSelectedChunkId] = useState<string | null>(null);
  const [selectedVersionId, setSelectedVersionId] = useState<string | null>(
    null,
  );
  const detail = sourceQuery.data;

  if (sourceQuery.isPending && !detail) {
    return <LoadingState label={t("sourceDetail.loading")} />;
  }
  if (sourceQuery.isError || !detail) {
    return (
      <div className="source-detail-page">
        <Button
          appearance="subtle"
          icon={<ArrowLeftRegular />}
          onClick={() => navigate(knowledgePath)}
        >
          {t("sourceDetail.back")}
        </Button>
        <ErrorState
          title={t("sourceDetail.loadError")}
          detail={t("sourceDetail.loadErrorDetail")}
          onRetry={() => void sourceQuery.refetch()}
        />
      </div>
    );
  }
  const { source, versions, chunks, facts, import_jobs: jobs, impact } = detail;
  const latestJob = jobs.at(-1);
  const isPdf = source.name.toLowerCase().endsWith(".pdf");
  const isDocx = source.name.toLowerCase().endsWith(".docx");
  const isXlsx = source.name.toLowerCase().endsWith(".xlsx");
  const format: SourceFormat = isPdf
    ? "pdf"
    : isDocx
      ? "docx"
      : isXlsx
        ? "xlsx"
        : "other";
  const unitFor = (count: number) =>
    t(`sourceDetail.unit.${format}`, { count });
  const completed = latestJob?.completed_units ?? 0;
  const failed = latestJob?.failed_units ?? 0;
  const progressValues = {
    completed,
    failed,
    completedUnit: unitFor(completed),
    failedUnit: unitFor(failed),
  };
  const currentVersion =
    versions.find(
      (version) => version.source_version_id === source.current_version_id,
    ) ?? (source.current_version_id ? undefined : versions.at(-1));
  const activeVersionId =
    selectedVersionId ?? currentVersion?.source_version_id;
  const visibleChunks = activeVersionId
    ? chunks.filter((chunk) => chunk.source_version_id === activeVersionId)
    : chunks;
  const selectedChunk = visibleChunks.find(
    (chunk) => chunk.chunk_id === selectedChunkId,
  );
  const isParsing =
    latestJob?.status === "queued" || latestJob?.status === "running";
  const mayRetry =
    canRetry &&
    latestJob &&
    (latestJob.status === "partial" || latestJob.status === "failed");
  // This only chooses presentation priority; the content endpoint remains
  // authoritative for the actual editable representation and permissions.
  const textFirst =
    source.kind === "text" ||
    currentVersion?.representation === "authored_text" ||
    /\.(md|markdown|txt)$/i.test(source.name);
  const sourceTextPanel = activeVersionId && tenantId && projectId && (
    <SourceTextRevisionPanel
      tenantId={tenantId}
      projectId={projectId}
      source={source}
      selectedVersionId={activeVersionId}
      canEdit={Boolean(canRetry)}
      onViewLatest={() => {
        setSelectedVersionId(null);
        setSelectedChunkId(null);
        void sourceQuery.refetch();
      }}
    />
  );

  return (
    <div className="source-detail-page">
      <section className="page-hero source-detail-hero">
        <div>
          <p className="eyebrow">{t("sourceDetail.eyebrow")}</p>
          <Link className="back-link" to={knowledgePath}>
            <ArrowLeftRegular /> {t("sourceDetail.back")}
          </Link>
          <h1>{source.name}</h1>
          <div className="source-status-line">
            {source.purpose === "internal"
              ? t("sourceDetail.internal")
              : t("sourceDetail.public")}{" "}
            · {t("sourceDetail.currentVersion")}{" "}
            {currentVersion
              ? currentVersion.version
              : t("sourceDetail.noVersion")}{" "}
            ·{" "}
            <StatusPill
              status={statusKind(
                latestJob?.status ?? source.import_status ?? source.state,
              )}
              text={importStatusLabel(
                latestJob?.status ?? source.import_status ?? source.state,
                t,
              )}
            />
          </div>
        </div>
        <div className="source-action-group">
          <Button
            appearance="secondary"
            icon={<ArrowSyncRegular />}
            disabled={!mayRetry || retryJob.isPending}
            title={
              !canRetry
                ? t("sourceDetail.readOnly")
                : !mayRetry
                  ? t("sourceDetail.retryUnavailable")
                  : t("sourceDetail.retryHint")
            }
            onClick={() => {
              if (latestJob && mayRetry) {
                void retryJob.mutateAsync(latestJob.import_job_id).catch(() => {
                  // The mutation exposes the scoped failure in the inline notice.
                });
              }
            }}
          >
            {retryJob.isPending
              ? t("sourceDetail.retrying")
              : t("sourceDetail.retry")}
          </Button>
        </div>
      </section>
      {sourceQuery.isFetching && (
        <MessageBar intent="info">
          <MessageBarBody>{t("sourceDetail.refreshing")}</MessageBarBody>
        </MessageBar>
      )}
      {retryJob.isError && (
        <MessageBar intent="error">
          <MessageBarBody>{t("sourceDetail.retryError")}</MessageBarBody>
        </MessageBar>
      )}
      {retryJob.isSuccess && (
        <MessageBar intent="info">
          <MessageBarBody>{t("sourceDetail.retryAccepted")}</MessageBarBody>
        </MessageBar>
      )}
      {isParsing && (
        <MessageBar intent="info">
          <MessageBarBody>
            {t(
              latestJob.status === "queued"
                ? "sourceDetail.queuedProgress"
                : "sourceDetail.runningProgress",
              progressValues,
            )}
          </MessageBarBody>
        </MessageBar>
      )}
      {latestJob?.status === "partial" && (
        <MessageBar intent="warning">
          <MessageBarBody>
            <b>{t("sourceDetail.partial")}</b>
            <span>{t("sourceDetail.partialProgress", progressValues)}</span>
          </MessageBarBody>
        </MessageBar>
      )}
      {latestJob?.status === "failed" && (
        <MessageBar intent="error">
          <MessageBarBody>
            {t("sourceDetail.failedProgress", progressValues)}
          </MessageBarBody>
        </MessageBar>
      )}
      {latestJob?.status === "succeeded" && isPdf && (
        <MessageBar intent="success">
          <MessageBarBody>
            {t("sourceDetail.pdfComplete", progressValues)}
          </MessageBarBody>
        </MessageBar>
      )}
      {latestJob?.status === "succeeded" && (isDocx || isXlsx) && (
        <MessageBar intent="success">
          <MessageBarBody>
            {t("sourceDetail.officeComplete", progressValues)}
          </MessageBarBody>
        </MessageBar>
      )}
      {latestJob?.errors && latestJob.errors.length > 0 && (
        <section
          aria-label={t(
            isPdf ? "sourceDetail.failedPages" : "sourceDetail.failedUnits",
          )}
        >
          <b>
            {t(isPdf ? "sourceDetail.failedPages" : "sourceDetail.failedUnits")}
          </b>
          <ul>
            {latestJob.errors.map((error, index) => (
              <li key={`${error.unit ?? "unit"}-${index}`}>
                {safeFailure(error, error.format ?? format, t)}
              </li>
            ))}
          </ul>
        </section>
      )}
      <section
        className={`source-detail-workbench${textFirst ? " source-detail-workbench--text-first" : ""}`}
      >
        {textFirst && sourceTextPanel}
        <Card className="source-original-panel" style={{ minWidth: 0 }}>
          <div className="knowledge-panel-heading">
            <div>
              <h2>{t("sourceDetail.originals")}</h2>
              <p>{t("sourceDetail.originalsHint")}</p>
            </div>
            <Badge appearance="tint">
              {t("sourceDetail.chunkCount", { count: visibleChunks.length })}
            </Badge>
          </div>
          {versions.length > 0 && (
            <label>
              {t("sourceDetail.viewVersion")}{" "}
              <Select
                aria-label={t("sourceDetail.viewVersion")}
                value={activeVersionId ?? ""}
                onChange={(_, data) => {
                  setSelectedVersionId(data.value);
                  setSelectedChunkId(null);
                }}
              >
                {versions.map((version) => (
                  <option
                    key={version.source_version_id}
                    value={version.source_version_id}
                  >
                    v{version.version}
                    {version.source_version_id === source.current_version_id
                      ? t("sourceDetail.versionCurrent")
                      : t("sourceDetail.versionHistory")}
                  </option>
                ))}
              </Select>
            </label>
          )}
          {selectedVersionId &&
            selectedVersionId !== source.current_version_id && (
              <p>{t("sourceDetail.viewingHistory")}</p>
            )}
          {visibleChunks.length === 0 ? (
            <EmptyState
              title={t("sourceDetail.noExtractedText")}
              detail={t("sourceDetail.noExtractedTextHint")}
            />
          ) : (
            <ol className="source-chunk-list">
              {visibleChunks.map((chunk) => (
                <li key={chunk.chunk_id}>
                  <button
                    type="button"
                    className={
                      selectedChunkId === chunk.chunk_id ? "selected" : ""
                    }
                    onClick={() => setSelectedChunkId(chunk.chunk_id)}
                  >
                    <small>
                      {t("sourceDetail.chunk", { index: chunk.ordinal + 1 })} ·{" "}
                      {t(`sourceDetail.kind.${chunk.kind}`, {
                        defaultValue: chunk.kind,
                      })}
                      {chunk.extraction_method ===
                      "deterministic_csv_evidence_v1"
                        ? ` · ${t("sourceDetail.generatedSlice")}`
                        : ""}
                    </small>
                    <span style={{ overflowWrap: "anywhere" }}>
                      {chunk.text}
                    </span>
                    <KnowledgeLocator locator={chunk.locator} />
                  </button>
                </li>
              ))}
            </ol>
          )}
        </Card>
        {!textFirst && sourceTextPanel}
        <Card className="source-extraction-panel" style={{ minWidth: 0 }}>
          <div className="knowledge-panel-heading">
            <div>
              <h2>{t("sourceDetail.results")}</h2>
              <p>{t("sourceDetail.resultsHint")}</p>
            </div>
            {isParsing && (
              <Spinner size="tiny" label={t("sourceDetail.processing")} />
            )}
          </div>
          {selectedChunk && (
            <section className="source-selected-chunk">
              <b>{t("sourceDetail.selectedPosition")}</b>
              <KnowledgeLocator locator={selectedChunk.locator} />
              <small>
                {selectedChunk.extraction_method
                  ? t("sourceDetail.extractionMethod", {
                      method:
                        selectedChunk.extraction_method ===
                        "deterministic_csv_evidence_v1"
                          ? t("sourceDetail.csvSliceMethod")
                          : selectedChunk.extraction_method ===
                              "deterministic_paragraph_v1"
                            ? t("sourceDetail.paragraphMethod")
                            : selectedChunk.extraction_method,
                    })
                  : t("sourceDetail.extractionPending")}
              </small>
              {selectedChunk.kind === "table" &&
                selectedChunk.locator?.kind === "csv" &&
                selectedChunk.extraction_method !==
                  "deterministic_csv_evidence_v1" && (
                  <CsvEvidenceTable text={selectedChunk.text} />
                )}
              {(selectedChunk.locator?.kind === "docx" ||
                selectedChunk.locator?.kind === "xlsx") && (
                <OfficeEvidenceTable
                  selected={selectedChunk}
                  chunks={visibleChunks}
                />
              )}
              {selectedChunk.extraction_method ===
                "deterministic_csv_evidence_v1" && (
                <p>{t("sourceDetail.generatedSliceHint")}</p>
              )}
            </section>
          )}
          {facts.length === 0 ? (
            <p className="source-empty-inline">{t("sourceDetail.noFacts")}</p>
          ) : (
            <ul className="source-fact-list">
              {facts.map((fact) => (
                <li key={fact.fact_id}>
                  <div>
                    <b>{fact.attribute}</b>
                    <span>
                      {typeof fact.typed_value === "object"
                        ? JSON.stringify(fact.typed_value)
                        : String(fact.typed_value ?? "—")}
                      {fact.unit ? ` ${fact.unit}` : ""}
                    </span>
                    <small>
                      {[fact.model, fact.market, fact.currency]
                        .filter(Boolean)
                        .join(" · ") || t("sourceDetail.scopeUnknown")}
                    </small>
                  </div>
                  <StatusPill
                    status={statusKind(fact.status)}
                    text={fact.status}
                  />
                  {fact.evidence_refs[0] && (
                    <Button
                      appearance="subtle"
                      size="small"
                      onClick={() =>
                        setSelectedChunkId(
                          fact.evidence_refs[0]?.chunk_id ?? null,
                        )
                      }
                    >
                      {t("sourceDetail.locate")}
                    </Button>
                  )}
                </li>
              ))}
            </ul>
          )}
        </Card>
        <Card className="source-impact-panel" style={{ minWidth: 0 }}>
          <h2>{t("sourceDetail.impact")}</h2>
          <dl className="source-metadata">
            <div>
              <dt>{t("sourceDetail.purpose")}</dt>
              <dd>
                {source.purpose === "internal"
                  ? t("sourceDetail.internalUse")
                  : t("sourceDetail.public")}
              </dd>
            </div>
            <div>
              <dt>{t("sourceDetail.version")}</dt>
              <dd>
                {currentVersion
                  ? `v${currentVersion.version}`
                  : t("sourceDetail.noUsableVersion")}
              </dd>
            </div>
            <div>
              <dt>{t("sourceDetail.progress")}</dt>
              <dd>
                {latestJob
                  ? t("sourceDetail.progressDetail", {
                      stage: t(`sourceDetail.stage.${latestJob.stage}`, {
                        defaultValue: latestJob.stage,
                      }),
                      status: importStatusLabel(latestJob.status, t),
                      ...progressValues,
                    })
                  : t("sourceDetail.awaitingImport")}
              </dd>
            </div>
            <div>
              <dt>{t("sourceDetail.hash")}</dt>
              <dd>
                {currentVersion?.content_sha256 ??
                  t("sourceDetail.awaitingHash")}
              </dd>
            </div>
          </dl>
          {impact.document_manifest_items?.length ||
          impact.content?.length ||
          impact.publications?.length ? (
            <section className="source-impact-list">
              <b>{t("sourceDetail.relatedContent")}</b>
              {impact.document_manifest_items?.map((item) => (
                <p key={item.id}>
                  <StatusPill
                    status={statusKind(item.status)}
                    text={item.status}
                  />{" "}
                  {t("sourceDetail.document")}：{item.label}
                </p>
              ))}
              {impact.content?.map((item) => (
                <p key={item.id}>
                  <StatusPill
                    status={statusKind(item.status ?? "processing")}
                    text={item.status ?? "processing"}
                  />{" "}
                  {t("sourceDetail.mainDocument")}：{item.title}
                  {item.revision ? ` v${item.revision}` : ""}
                </p>
              ))}
              {impact.publications?.map((item) => (
                <p key={item.id}>
                  <StatusPill
                    status={statusKind(item.status)}
                    text={item.status}
                  />{" "}
                  {t("sourceDetail.publication")}：{item.label}
                </p>
              ))}
            </section>
          ) : (
            <p className="source-empty-inline">
              {t("sourceDetail.noRelatedContent")}
            </p>
          )}
          {currentVersion?.original_url && (
            <a
              href={currentVersion.original_url}
              target="_blank"
              rel="noreferrer"
            >
              <OpenRegular /> {t("sourceDetail.openOriginal")}
            </a>
          )}
        </Card>
      </section>
    </div>
  );
}
