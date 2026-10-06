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
  ocr_required: "该页没有可提取的文字层；扫描内容需要 OCR",
  empty_text: "该页未提取到文字",
  parse_failed: "该页解析失败",
  page_limit: "页面超过解析限制",
  invalid_pdf: "PDF 文件无效或无法读取",
  encrypted_pdf: "PDF 已加密，无法解析",
};

const officeFailureReasons: Record<string, string> = {
  invalid_docx: "DOCX 文件无效或无法读取",
  invalid_xlsx: "XLSX 文件无效或无法读取",
  encrypted_office: "Office 文件已加密，无法解析",
  parse_failed: "该解析单元失败",
  unit_limit: "解析单元超过限制",
  unsupported_content: "该解析单元含不支持的内容",
  empty_text: "该解析单元没有可提取文字",
};

type SourceFormat = "pdf" | "docx" | "xlsx" | "other";

function safeFailure(
  error: NonNullable<ImportJob["errors"]>[number],
  format: SourceFormat,
) {
  if (format === "docx" || format === "xlsx") {
    const id =
      Number.isInteger(error.unit_id) && (error.unit_id ?? -1) >= 0
        ? `解析单元 ${(error.unit_id ?? 0) + 1}：`
        : "";
    return `${id}${officeFailureReasons[error.code ?? ""] ?? "Office 解析失败；请检查该单元或文件"}`;
  }
  if (format !== "pdf") return "解析失败；请检查该单元或文件";
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
  const position = page ? `第 ${page} 页：` : "";
  return `${position}${pdfFailureReasons[error.code ?? ""] ?? "解析失败；请检查该单元或文件"}`;
}

function importStatusLabel(value: string | null | undefined) {
  const labels: Record<string, string> = {
    queued: "等待解析",
    running: "正在解析",
    partial: "部分完成",
    succeeded: "解析完成",
    failed: "解析失败",
    cancelled: "已取消",
  };
  return labels[value ?? ""] ?? value ?? "等待处理";
}

export function SourceDetailPage() {
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
    return <LoadingState label="正在加载来源详情" />;
  }
  if (sourceQuery.isError || !detail) {
    return (
      <div className="source-detail-page">
        <Button
          appearance="subtle"
          icon={<ArrowLeftRegular />}
          onClick={() => navigate(knowledgePath)}
        >
          返回资料中心
        </Button>
        <ErrorState
          title="无法加载这份资料"
          detail="资料可能已被移除、当前项目无权访问，或服务暂时不可用。"
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
  const unitLabel = isPdf
    ? "页"
    : isDocx
      ? "个正文区块"
      : isXlsx
        ? "个工作表行区块"
        : "个单元";
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

  return (
    <div className="source-detail-page">
      <section className="page-hero source-detail-hero">
        <div>
          <p className="eyebrow">资料详情</p>
          <Link className="back-link" to={knowledgePath}>
            <ArrowLeftRegular /> 返回资料中心
          </Link>
          <h1>{source.name}</h1>
          <div className="source-status-line">
            {source.purpose === "internal" ? "内部资料" : "公开资料"} · 当前版本
            {currentVersion ? ` ${currentVersion.version}` : "尚未形成"} ·{" "}
            <StatusPill
              status={statusKind(
                latestJob?.status ?? source.import_status ?? source.state,
              )}
              text={importStatusLabel(
                latestJob?.status ?? source.import_status ?? source.state,
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
                ? "当前成员只有读取权限。"
                : !mayRetry
                  ? "只有部分完成或失败的最新任务可以重试。"
                  : "仅重试失败的解析单元；不会删除已完成的证据或旧版本。"
            }
            onClick={() => {
              if (latestJob && mayRetry) {
                void retryJob.mutateAsync(latestJob.import_job_id).catch(() => {
                  // The mutation exposes the scoped failure in the inline notice.
                });
              }
            }}
          >
            {retryJob.isPending ? "正在受理重试…" : "重试失败部分"}
          </Button>
        </div>
      </section>
      {sourceQuery.isFetching && (
        <MessageBar intent="info">
          <MessageBarBody>
            正在更新来源处理进度，当前已显示的版本与片段仍可查看。
          </MessageBarBody>
        </MessageBar>
      )}
      {retryJob.isError && (
        <MessageBar intent="error">
          <MessageBarBody>
            无法受理重试。请确认任务仍为部分完成或失败，并检查当前项目权限后重试。
          </MessageBarBody>
        </MessageBar>
      )}
      {retryJob.isSuccess && (
        <MessageBar intent="info">
          <MessageBarBody>
            重试已受理；只会补处理失败单元，已有证据和历史版本保持不变。
          </MessageBarBody>
        </MessageBar>
      )}
      {isParsing && (
        <MessageBar intent="info">
          <MessageBarBody>
            {latestJob.status === "queued" ? "已受理，等待解析" : "正在解析"}。
            当前已完成 {latestJob.completed_units ?? 0} {unitLabel}，失败{" "}
            {latestJob.failed_units ?? 0} {unitLabel}
            ；完成前不代表全文已可用，知识版本只在实际发布后更新。
          </MessageBarBody>
        </MessageBar>
      )}
      {latestJob?.status === "partial" && (
        <MessageBar intent="warning">
          <MessageBarBody>
            <b>部分处理完成</b>
            <span>
              已完成 {latestJob.completed_units ?? 0} {unitLabel}，失败{" "}
              {latestJob.failed_units ?? 0} {unitLabel}
              。仅成功单元的证据可用；失败单元不会计入。
            </span>
          </MessageBarBody>
        </MessageBar>
      )}
      {latestJob?.status === "failed" && (
        <MessageBar intent="error">
          <MessageBarBody>
            解析失败：已完成 {latestJob.completed_units ?? 0} {unitLabel}，失败{" "}
            {latestJob.failed_units ?? 0} {unitLabel}
            。旧来源版本与已保存证据不会因此删除。
          </MessageBarBody>
        </MessageBar>
      )}
      {latestJob?.status === "succeeded" && isPdf && (
        <MessageBar intent="success">
          <MessageBarBody>
            PDF 页面文字解析完成，共 {latestJob.completed_units ?? 0}{" "}
            页。页面文字可按页定位； 这不表示扫描页 OCR、结构化事实或 PDF
            原件可视预览已完成。
          </MessageBarBody>
        </MessageBar>
      )}
      {latestJob?.status === "succeeded" && (isDocx || isXlsx) && (
        <MessageBar intent="success">
          <MessageBarBody>
            {isDocx ? "DOCX 正文结构" : "XLSX 工作表单元格"}解析完成，共{" "}
            {latestJob.completed_units ?? 0} {unitLabel}
            。证据可按结构化位置查看； 不提供 Office 原件预览或原文高亮。
          </MessageBarBody>
        </MessageBar>
      )}
      {latestJob?.errors && latestJob.errors.length > 0 && (
        <section aria-label={isPdf ? "失败页面与原因" : "失败解析单元与原因"}>
          <b>{isPdf ? "失败页面与原因" : "失败解析单元与原因"}</b>
          <ul>
            {latestJob.errors.map((error, index) => (
              <li key={`${error.unit ?? "unit"}-${index}`}>
                {safeFailure(error, error.format ?? format)}
              </li>
            ))}
          </ul>
        </section>
      )}
      <section className="source-detail-workbench">
        <Card className="source-original-panel" style={{ minWidth: 0 }}>
          <div className="knowledge-panel-heading">
            <div>
              <h2>原文与快照</h2>
              <p>
                点击片段可查看结构化定位；按来源版本分别浏览证据。
                当前只显示提取内容，不提供原件预览或区域高亮。
              </p>
            </div>
            <Badge appearance="tint">{visibleChunks.length} 个片段</Badge>
          </div>
          {versions.length > 0 && (
            <label>
              查看证据版本{" "}
              <Select
                aria-label="查看证据版本"
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
                      ? "（当前）"
                      : "（历史不可变版本）"}
                  </option>
                ))}
              </Select>
            </label>
          )}
          {selectedVersionId &&
            selectedVersionId !== source.current_version_id && (
              <p>正在查看已保存的历史证据；新的解析不会覆盖这个版本。</p>
            )}
          {visibleChunks.length === 0 ? (
            <EmptyState
              title="原文尚未可用"
              detail="资料正在获取或解析；不会以空白内容冒充已解析原文。"
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
                      片段 {chunk.ordinal + 1} · {chunk.kind}
                      {chunk.extraction_method ===
                      "deterministic_csv_evidence_v1"
                        ? " · 生成证据分片"
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
        {activeVersionId && tenantId && projectId && (
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
        )}
        <Card className="source-extraction-panel" style={{ minWidth: 0 }}>
          <div className="knowledge-panel-heading">
            <div>
              <h2>提取结果</h2>
              <p>证据按所选的不可变来源版本查看；事实保留服务端记录。</p>
            </div>
            {isParsing && <Spinner size="tiny" label="正在处理" />}
          </div>
          {selectedChunk && (
            <section className="source-selected-chunk">
              <b>当前定位</b>
              <KnowledgeLocator locator={selectedChunk.locator} />
              <small>
                {selectedChunk.extraction_method
                  ? `提取方式：${selectedChunk.extraction_method}`
                  : "正在等待提取方式记录"}
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
                <p>
                  这是供生成使用的有界证据分片；完整记录仍保存在原始片段中。
                </p>
              )}
            </section>
          )}
          {facts.length === 0 ? (
            <p className="source-empty-inline">尚未从此来源提取到事实。</p>
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
                        .join(" · ") || "适用范围待确认"}
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
                      定位
                    </Button>
                  )}
                </li>
              ))}
            </ul>
          )}
        </Card>
        <Card className="source-impact-panel" style={{ minWidth: 0 }}>
          <h2>版本、用途与影响</h2>
          <dl className="source-metadata">
            <div>
              <dt>来源用途</dt>
              <dd>
                {source.purpose === "internal"
                  ? "内部资料，不进入公开生成"
                  : "公开资料"}
              </dd>
            </div>
            <div>
              <dt>版本</dt>
              <dd>
                {currentVersion
                  ? `v${currentVersion.version}`
                  : "尚未形成可用版本"}
              </dd>
            </div>
            <div>
              <dt>处理进度</dt>
              <dd>
                {latestJob
                  ? `${latestJob.stage}：${importStatusLabel(latestJob.status)}，已完成 ${latestJob.completed_units ?? 0} ${unitLabel}，失败 ${latestJob.failed_units ?? 0} ${unitLabel}`
                  : "等待导入任务"}
              </dd>
            </div>
            <div>
              <dt>内容哈希</dt>
              <dd>{currentVersion?.content_sha256 ?? "等待完成核验"}</dd>
            </div>
          </dl>
          {impact.document_manifest_items?.length ||
          impact.content?.length ||
          impact.publications?.length ? (
            <section className="source-impact-list">
              <b>影响链</b>
              {impact.document_manifest_items?.map((item) => (
                <p key={item.id}>
                  <StatusPill
                    status={statusKind(item.status)}
                    text={item.status}
                  />{" "}
                  文档清单：{item.label}
                </p>
              ))}
              {impact.content?.map((item) => (
                <p key={item.id}>
                  <StatusPill
                    status={statusKind(item.status ?? "processing")}
                    text={item.status ?? "processing"}
                  />{" "}
                  主文档：{item.title}
                  {item.revision ? ` v${item.revision}` : ""}
                </p>
              ))}
              {impact.publications?.map((item) => (
                <p key={item.id}>
                  <StatusPill
                    status={statusKind(item.status)}
                    text={item.status}
                  />{" "}
                  发布证据：{item.label}
                </p>
              ))}
            </section>
          ) : (
            <p className="source-empty-inline">
              还没有引用这份资料的文档、渠道变体或发布证据。
            </p>
          )}
          {currentVersion?.original_url && (
            <a
              href={currentVersion.original_url}
              target="_blank"
              rel="noreferrer"
            >
              <OpenRegular /> 打开原始 URL
            </a>
          )}
        </Card>
      </section>
    </div>
  );
}
