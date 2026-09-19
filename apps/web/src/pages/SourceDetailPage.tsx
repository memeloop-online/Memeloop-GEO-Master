import { useMemo, useState } from "react";
import {
  Badge,
  Button,
  Card,
  MessageBar,
  MessageBarBody,
  Spinner,
} from "@fluentui/react-components";
import {
  ArrowLeftRegular,
  ArrowSyncRegular,
  DeleteRegular,
  DocumentArrowUpRegular,
  OpenRegular,
} from "@fluentui/react-icons";
import { Link, useNavigate, useParams } from "react-router-dom";
import { useSourceQuery } from "../api/knowledge";
import { ErrorState, EmptyState, LoadingState } from "../components/AsyncState";
import { KnowledgeLocator } from "../components/KnowledgeLocator";
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

export function SourceDetailPage() {
  const { tenantId, projectId, id } = useParams();
  const navigate = useNavigate();
  const sourceQuery = useSourceQuery(tenantId, projectId, id);
  const knowledgePath =
    tenantId && projectId
      ? `/app/${encodeURIComponent(tenantId)}/${encodeURIComponent(projectId)}/knowledge`
      : "../knowledge";
  const [selectedChunkId, setSelectedChunkId] = useState<string | null>(null);
  const detail = sourceQuery.data;
  const selectedChunk = useMemo(
    () => detail?.chunks.find((chunk) => chunk.chunk_id === selectedChunkId),
    [detail?.chunks, selectedChunkId],
  );

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
  const partialJobs = jobs.filter((job) => job.status === "partial");
  const failedJobs = jobs.filter((job) => job.status === "failed");

  return (
    <div className="source-detail-page">
      <section className="page-hero source-detail-hero">
        <div>
          <p className="eyebrow">P04 · 资料详情</p>
          <Link className="back-link" to={knowledgePath}>
            <ArrowLeftRegular /> 返回资料中心
          </Link>
          <h1>{source.name}</h1>
          <div className="source-status-line">
            {source.purpose === "internal" ? "内部资料" : "公开资料"} · 当前版本
            {versions[0] ? ` ${versions[0].version}` : "尚未形成"} ·{" "}
            <StatusPill
              status={statusKind(source.import_status ?? source.state)}
              text={source.import_status ?? source.state}
            />
          </div>
        </div>
        <div className="source-action-group">
          <Button
            appearance="secondary"
            icon={<DocumentArrowUpRegular />}
            disabled
            title="替换文件端点尚未接入；系统不会假装已创建新版本。"
          >
            替换文件
          </Button>
          <Button
            appearance="secondary"
            icon={<ArrowSyncRegular />}
            disabled
            title="重试解析端点尚未接入；系统不会假装已经重试。"
          >
            重试失败部分
          </Button>
          <Button
            appearance="secondary"
            icon={<DeleteRegular />}
            disabled
            title="移除端点尚未接入；系统不会假装已删除来源。"
          >
            移除来源
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
      {partialJobs.length > 0 && (
        <MessageBar intent="warning">
          <MessageBarBody>
            <b>部分处理完成</b>
            <span>
              已完成{" "}
              {partialJobs.reduce(
                (sum, job) => sum + (job.completed_units ?? 0),
                0,
              )}
              个单元，失败{" "}
              {partialJobs.reduce(
                (sum, job) => sum + (job.failed_units ?? 0),
                0,
              )}
              个单元。事实数只代表已成功提取的部分。
            </span>
          </MessageBarBody>
        </MessageBar>
      )}
      {failedJobs.length > 0 && (
        <MessageBar intent="error">
          <MessageBarBody>
            {failedJobs.length} 个处理任务失败：
            {failedJobs
              .flatMap((job) => job.errors ?? [])
              .map((error) => error.message)
              .filter(Boolean)
              .join("；") || "请查看任务记录中的实际原因。"}
          </MessageBarBody>
        </MessageBar>
      )}
      <section className="source-detail-workbench">
        <Card className="source-original-panel">
          <div className="knowledge-panel-heading">
            <div>
              <h2>原文与快照</h2>
              <p>
                点击片段会定位到它的结构化位置；不会把定位信息简化为模糊页码。
              </p>
            </div>
            <Badge appearance="tint">{chunks.length} 个片段</Badge>
          </div>
          {detail.original_text ? (
            <pre className="source-original-text">{detail.original_text}</pre>
          ) : chunks.length === 0 ? (
            <EmptyState
              title="原文尚未可用"
              detail="资料正在获取或解析；不会以空白内容冒充已解析原文。"
            />
          ) : (
            <ol className="source-chunk-list">
              {chunks.map((chunk) => (
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
                    </small>
                    <span>{chunk.text}</span>
                    <KnowledgeLocator locator={chunk.locator} />
                  </button>
                </li>
              ))}
            </ol>
          )}
        </Card>
        <Card className="source-extraction-panel">
          <div className="knowledge-panel-heading">
            <div>
              <h2>提取结果</h2>
              <p>事实和片段均绑定当前来源版本。</p>
            </div>
            {source.import_status === "running" && (
              <Spinner size="tiny" label="正在处理" />
            )}
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
        <Card className="source-impact-panel">
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
                {versions[0] ? `v${versions[0].version}` : "尚未形成版本"}
              </dd>
            </div>
            <div>
              <dt>处理进度</dt>
              <dd>
                {jobs.length
                  ? jobs.map((job) => `${job.stage}：${job.status}`).join("；")
                  : "等待导入任务"}
              </dd>
            </div>
            <div>
              <dt>内容哈希</dt>
              <dd>{versions[0]?.content_sha256 ?? "等待完成核验"}</dd>
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
          {versions[0]?.original_url && (
            <a href={versions[0].original_url} target="_blank" rel="noreferrer">
              <OpenRegular /> 打开原始 URL
            </a>
          )}
        </Card>
      </section>
    </div>
  );
}
