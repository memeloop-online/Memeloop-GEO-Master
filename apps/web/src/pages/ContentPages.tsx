import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { formatUiDate } from "../i18n";
import {
  Badge,
  Button,
  Card,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
} from "@fluentui/react-components";
import {
  Link,
  useNavigate,
  useParams,
  useSearchParams,
} from "react-router-dom";
import { ApiError } from "../api/client";
import {
  type ContentItem,
  type ContentRevision,
  type RichNode,
  type StructuredDocument,
  exportContentRevision,
  useAppendContentRevisionMutation,
  useContentAssetQuery,
  useContentCycleQuery,
  useContentExecutionActionMutation,
  useContentExecutionsQuery,
  useContentItemsQuery,
  useContentRevisionsQuery,
  useForkReusedContentItemMutation,
  useStartContentExecutionMutation,
} from "../api/content";
import { useDocumentManifestQuery } from "../api/documentManifests";
import { useAuth } from "../auth/AuthProvider";
import { membershipForTenant } from "../auth/types";
import { EmptyState, ErrorState, LoadingState } from "../components/AsyncState";
import {
  documentToEditor,
  StructuredContentEditor,
} from "../components/StructuredContentEditor";

const stateLabels: Record<ContentItem["status"], string> = {
  pending: "等待准备",
  prepared: "简报已准备",
  drafted: "草稿待检查",
  needs_repair: "待自动修正",
  ready: "正文就绪",
  blocked: "阻断",
  deferred: "延后",
  not_applicable: "不适用",
  cancelled: "已取消",
};
const planLabels = {
  planned: "已规划",
  blocked: "规划阻断",
  deferred: "规划延后",
  not_applicable: "规划不适用",
};

function mayEdit(role: string | undefined) {
  return role === "tenant_admin" || role === "member";
}

function containsMedia(node: RichNode): boolean {
  return node.type === "media" || (node.content?.some(containsMedia) ?? false);
}

function apiTitle(error: Error, fallback: string) {
  if (error instanceof ApiError) {
    if (error.status === 403) return "权限不足";
    if (error.code === "capability_missing") return "内容生成能力尚未配置";
  }
  return fallback;
}

function ItemCard({
  item,
  planned,
  executionState,
}: {
  item?: ContentItem;
  executionState: "absent" | "loading" | "unavailable" | "loaded";
  planned: {
    document_manifest_item_id: string;
    document_key: string;
    content_type: string;
    market: string;
    language: string;
    state: "planned" | "blocked" | "deferred" | "not_applicable";
    block_reason?: string | null;
    source_version_refs: string[];
  };
}) {
  return (
    <li>
      <Card className="panel-card">
        <h3>{planned.content_type}</h3>
        <div>
          {planned.market} · {planned.language} ·{" "}
          <Badge
            appearance="tint"
            color={
              item?.status === "blocked" || planned.state === "blocked"
                ? "danger"
                : "informative"
            }
          >
            {item ? stateLabels[item.status] : planLabels[planned.state]}
          </Badge>
        </div>
        <dl>
          <dt>文档键</dt>
          <dd>{planned.document_key}</dd>
          <dt>清单项</dt>
          <dd>{planned.document_manifest_item_id}</dd>
          <dt>来源版本</dt>
          <dd>
            {planned.source_version_refs.length
              ? planned.source_version_refs.join("、")
              : "未记录"}
          </dd>
          <dt>规划状态</dt>
          <dd>
            {planLabels[planned.state]}
            {planned.block_reason ? `：${planned.block_reason}` : ""}
          </dd>
          <dt>执行状态</dt>
          <dd>
            {item
              ? stateLabels[item.status]
              : executionState === "loading"
                ? "执行记录读取中"
                : executionState === "unavailable"
                  ? "执行记录暂不可用"
                  : executionState === "loaded"
                    ? "执行记录缺失，覆盖待核对"
                    : "尚无执行记录"}
            {item?.reason ? `：${item.reason}` : ""}
          </dd>
          {item &&
            (item.status === "needs_repair" ||
              (item.automatic_repair_count ?? 0) > 0) && (
              <>
                <dt>自动修正</dt>
                <dd>
                  已完成 {item.automatic_repair_count ?? 0} / 2 轮；
                  修正后将重新检查，检查通过前不交给发布。
                </dd>
              </>
            )}
          {item?.brief && (
            <>
              <dt>内容简报</dt>
              <dd>
                {item.brief.title} · {item.brief.objective}
              </dd>
            </>
          )}
          {item?.reuse_binding && (
            <>
              <dt>跨周期复用</dt>
              <dd>
                复用已检查版本；本轮仍有独立清单项和覆盖记录，正文与检查证据来自原资产。
                原执行 {item.reuse_binding.origin_execution_id} · 原清单项{" "}
                {item.reuse_binding.origin_item_id} · 检查{" "}
                {item.reuse_binding.check_id}
              </dd>
            </>
          )}
        </dl>
        {item?.reuse_binding ? (
          <>
            <Link
              to={`${encodeURIComponent(item.asset_id ?? item.reuse_binding.asset_id)}?reuse_execution_id=${encodeURIComponent(item.execution_id)}&reuse_item_id=${encodeURIComponent(item.item_id)}`}
            >
              {item.asset_id && item.asset_id !== item.reuse_binding.asset_id
                ? "查看本轮修订正文与版本"
                : "查看复用正文与原检查证据"}
            </Link>{" "}
            <Link to={encodeURIComponent(item.reuse_binding.asset_id)}>
              查看原资产（直接编辑将修改原资产）
            </Link>
          </>
        ) : item?.asset_id ? (
          <Link to={encodeURIComponent(item.asset_id)}>查看正文与版本</Link>
        ) : executionState === "loading" || executionState === "unavailable" ? (
          <p>正在读取内容状态。</p>
        ) : (
          <p>这篇内容尚未生成。</p>
        )}
      </Card>
    </li>
  );
}

function AssetsContent({
  tenantId,
  projectId,
}: {
  tenantId: string;
  projectId: string;
}) {
  const { t } = useTranslation();
  const { session } = useAuth();
  const role = membershipForTenant(session, tenantId)?.role;
  const [searchParams] = useSearchParams();
  const cycle = useContentCycleQuery(tenantId, projectId);
  const requestedCycleId = searchParams.get("cycle_id");
  const cycleId = requestedCycleId || cycle.data?.cycle_id;
  const handle =
    cycle.data && cycle.data.cycle_id === cycleId
      ? cycle.data.document_manifest
      : null;
  const manifestId =
    handle && typeof handle === "object" && "manifest_id" in handle
      ? String(handle.manifest_id)
      : undefined;
  const manifest = useDocumentManifestQuery(tenantId, projectId, manifestId);
  const executions = useContentExecutionsQuery(tenantId, projectId, cycleId);
  const active = executions.data?.find(
    (entry) =>
      entry.manifest_id === manifestId &&
      entry.manifest_revision === manifest.data?.revision,
  );
  const items = useContentItemsQuery(tenantId, projectId, active?.execution_id);
  const start = useStartContentExecutionMutation(
    tenantId,
    projectId,
    cycleId ?? "",
  );
  const resume = useContentExecutionActionMutation(
    tenantId,
    projectId,
    cycleId ?? "",
    "resume",
  );
  const cancel = useContentExecutionActionMutation(
    tenantId,
    projectId,
    cycleId ?? "",
    "cancel",
  );
  const [gapOnly, setGapOnly] = useState(false);

  if (cycle.isPending) return <LoadingState label="正在加载当前周期" />;
  if (cycle.isError)
    return (
      <ErrorState
        title={apiTitle(cycle.error, "无法读取当前周期")}
        detail={cycle.error.message}
        onRetry={() => void cycle.refetch()}
      />
    );
  if (!cycle.data)
    return (
      <EmptyState
        title="尚无当前周期"
        detail="项目启动后才会形成文档清单；内容资产不会由演示数据代替。"
      />
    );
  if (requestedCycleId && requestedCycleId !== cycle.data.cycle_id)
    return (
      <ErrorState
        title="不是当前周期"
        detail="当前内容页只提供当前周期的清单与执行；请返回当前计划。"
      />
    );

  const plan = manifest.data;
  const executionItems = new Map(
    items.data?.map((item) => [item.item_id, item]),
  );
  const plannedItems = plan?.items.filter((planned) => {
    if (!gapOnly) return true;
    const item = executionItems.get(planned.document_manifest_item_id);
    return !item || item.status !== "ready";
  });
  return (
    <div className="workbench-page">
      <section className="page-hero">
        <div>
          <p className="eyebrow">内容资产</p>
          <h1>{t("generatedEditor.cycleTitle")}</h1>
          <p>{t("generatedEditor.cycleDescription")}</p>
        </div>
        <Button
          onClick={() => {
            void Promise.all([
              cycle.refetch(),
              manifest.refetch(),
              executions.refetch(),
              items.refetch(),
            ]);
          }}
          disabled={cycle.isFetching || manifest.isFetching || items.isFetching}
        >
          刷新状态
        </Button>
      </section>
      {!mayEdit(role) && (
        <MessageBar intent="warning">
          <MessageBarBody>当前成员仅可查看内容与版本。</MessageBarBody>
        </MessageBar>
      )}
      {!manifestId && (
        <EmptyState
          title="本轮还没有文档清单"
          detail="先在计划页形成并封存知识文档清单。"
          action={<Link to="../campaigns/current">查看当前计划</Link>}
        />
      )}
      {manifestId && manifest.isPending && (
        <LoadingState label="正在加载冻结文档清单" />
      )}
      {manifest.isError && (
        <ErrorState
          title={apiTitle(manifest.error, "无法读取文档清单")}
          detail={manifest.error.message}
          onRetry={() => void manifest.refetch()}
        />
      )}
      {manifestId && !manifest.isPending && !manifest.isError && !plan && (
        <EmptyState
          title="清单尚未规划"
          detail="尚未生成内容计划，可在 AI 工作台描述你希望覆盖的话题。"
          action={<Link to="../campaigns/current">规划文档清单</Link>}
        />
      )}
      {plan && (
        <>
          <Card className="panel-card">
            <h2>{t("generatedEditor.planProgress")}</h2>
            <p>
              清单 {plan.manifest_id} · 修订 {plan.revision} ·{" "}
              {plan.sealed ? "已封存" : "未封存"} · 规划分母{" "}
              {plan.expected_count ?? "未知"}
            </p>
            <p>
              规划：已规划 {plan.coverage.planned} · 阻断{" "}
              {plan.coverage.blocked} · 延后 {plan.coverage.deferred} · 不适用{" "}
              {plan.coverage.not_applicable}
            </p>
            {active ? (
              <p>
                执行：{active.status} · 分母 {active.expected_count} · 就绪{" "}
                {active.coverage.ready} · 阻断 {active.coverage.blocked} · 延后{" "}
                {active.coverage.deferred} · 未完成 {active.coverage.incomplete}{" "}
                · 交接 {active.handoff_id ?? "尚未形成"}
              </p>
            ) : (
              <p>{t("generatedEditor.notStarted")}</p>
            )}
            {executions.isPending && (
              <LoadingState label="正在读取执行进度" compact />
            )}
            {executions.isError && (
              <ErrorState
                title={apiTitle(executions.error, "无法读取执行进度")}
                detail={executions.error.message}
                onRetry={() => void executions.refetch()}
              />
            )}
            {!active &&
              plan.sealed &&
              !executions.isPending &&
              !executions.isError && (
                <Button
                  appearance="primary"
                  disabled={!mayEdit(role) || start.isPending}
                  onClick={() => start.mutate()}
                >
                  {start.isPending ? "正在启动…" : "启动正文生成"}
                </Button>
              )}
            {start.isError && (
              <ErrorState
                title={apiTitle(start.error, "无法启动正文生成")}
                detail={start.error.message}
              />
            )}
            {active?.status === "running" && (
              <>
                <p>{t("generatedEditor.resumeDescription")}</p>
                <Button
                  disabled={
                    !mayEdit(role) || resume.isPending || cancel.isPending
                  }
                  onClick={() => resume.mutate(active.execution_id)}
                >
                  {resume.isPending ? "正在恢复…" : "恢复未完成分支"}
                </Button>{" "}
                <Button
                  disabled={
                    !mayEdit(role) || resume.isPending || cancel.isPending
                  }
                  onClick={() => cancel.mutate(active.execution_id)}
                >
                  {cancel.isPending ? "正在取消…" : "取消未完成分支"}
                </Button>
              </>
            )}
            {resume.isSuccess && (
              <MessageBar intent="info">
                <MessageBarBody>
                  恢复请求已受理；以刷新后的逐项执行记录为准。
                </MessageBarBody>
              </MessageBar>
            )}
            {cancel.isSuccess && (
              <MessageBar intent="warning">
                <MessageBarBody>
                  取消结果已持久化；已就绪版本保留，未完成分支以服务端状态为准。
                </MessageBarBody>
              </MessageBar>
            )}
            {resume.isError && (
              <ErrorState
                title={apiTitle(resume.error, "无法恢复正文执行")}
                detail={resume.error.message}
              />
            )}
            {cancel.isError && (
              <ErrorState
                title={apiTitle(cancel.error, "无法取消正文执行")}
                detail={cancel.error.message}
              />
            )}
          </Card>
          {items.isError && (
            <ErrorState
              title={apiTitle(items.error, "无法读取逐项执行结果")}
              detail={items.error.message}
              onRetry={() => void items.refetch()}
            />
          )}
          <section aria-label="全部文档分支">
            <h2>全部文档分支</h2>
            <label>
              <input
                type="checkbox"
                checked={gapOnly}
                onChange={(event) => setGapOnly(event.target.checked)}
              />{" "}
              仅看未就绪项
            </label>
            {items.isPending && active && (
              <LoadingState label="正在读取逐项进度" compact />
            )}
            {active &&
              items.isSuccess &&
              items.data.length !== active.expected_count && (
                <MessageBar intent="warning">
                  <MessageBarBody>
                    执行账本返回 {items.data.length} 项，冻结分母为{" "}
                    {active.expected_count}；未返回的分支仍保留在规划清单中。
                  </MessageBarBody>
                </MessageBar>
              )}
            {!plannedItems?.length ? (
              <p>{gapOnly ? "没有符合筛选的缺口。" : "清单没有文档分支。"}</p>
            ) : (
              <ul>
                {plannedItems.map((planned) => (
                  <ItemCard
                    key={planned.document_manifest_item_id}
                    planned={planned}
                    item={executionItems.get(planned.document_manifest_item_id)}
                    executionState={
                      !active
                        ? "absent"
                        : items.isPending
                          ? "loading"
                          : items.isError
                            ? "unavailable"
                            : "loaded"
                    }
                  />
                ))}
              </ul>
            )}
          </section>
        </>
      )}
    </div>
  );
}

export function ContentAssetsPage() {
  const { tenantId, projectId } = useParams();
  if (!tenantId || !projectId) return <ErrorState title="缺少项目标识" />;
  return (
    <AssetsContent
      key={`${tenantId}/${projectId}`}
      tenantId={tenantId}
      projectId={projectId}
    />
  );
}

function locatorText(locator: { kind: string; [key: string]: unknown }) {
  const fields = Object.entries(locator)
    .filter(([key]) => key !== "kind")
    .map(
      ([key, value]) =>
        `${key}: ${Array.isArray(value) ? value.join("/") : String(value)}`,
    );
  return `${locator.kind} · ${fields.join(" · ")}`;
}

function RevisionEditor({
  revision,
  tenantId,
  projectId,
  assetId,
  readonly,
  forkContext,
}: {
  revision: ContentRevision;
  tenantId: string;
  projectId: string;
  assetId: string;
  readonly: boolean;
  forkContext?: { executionId: string; itemId: string };
}) {
  const { t } = useTranslation();
  const unsupported = useMemo(() => {
    try {
      documentToEditor(revision.document);
      return false;
    } catch {
      return true;
    }
  }, [revision.document]);
  const navigate = useNavigate();
  const [draft, setDraft] = useState<StructuredDocument>(() =>
    structuredClone(revision.document),
  );
  const [baselineDocument, setBaselineDocument] = useState(revision.document);
  const changeGeneration = useRef(0);
  const [baseId, setBaseId] = useState(revision.revision_id);
  const [dirty, setDirty] = useState(false);
  const [conflicted, setConflicted] = useState(false);
  const [saved, setSaved] = useState(false);
  const [editorEpoch, setEditorEpoch] = useState(0);
  const [editorError, setEditorError] = useState<string | null>(null);
  const append = useAppendContentRevisionMutation(tenantId, projectId, assetId);
  const fork = useForkReusedContentItemMutation(
    tenantId,
    projectId,
    forkContext?.executionId ?? "",
    forkContext?.itemId ?? "",
  );
  const mutation = forkContext ? fork : append;

  useEffect(() => {
    if (!dirty && !saved && revision.revision_id !== baseId) {
      setDraft(structuredClone(revision.document));
      setBaselineDocument(revision.document);
      setBaseId(revision.revision_id);
      setEditorEpoch((epoch) => epoch + 1);
    }
    if (saved && revision.revision_id === baseId) setSaved(false);
  }, [baseId, dirty, revision, saved]);

  const update = (
    next:
      | StructuredDocument
      | ((current: StructuredDocument) => StructuredDocument),
  ) => {
    changeGeneration.current++;
    setDraft(next);
    setDirty(true);
    setSaved(false);
  };
  const save = () => {
    if (
      !dirty ||
      mutation.isPending ||
      conflicted ||
      editorError ||
      unsupported
    )
      return;
    const submittedGeneration = changeGeneration.current;
    mutation.mutate(
      { baseRevisionId: baseId, document: draft },
      {
        onSuccess: (result) => {
          setBaseId(result.revision_id);
          setBaselineDocument(result.document);
          if (submittedGeneration === changeGeneration.current) {
            setDirty(false);
            setSaved(true);
          }
          if (forkContext) {
            navigate(
              `/app/${encodeURIComponent(tenantId)}/${encodeURIComponent(projectId)}/content/${encodeURIComponent(result.asset_id)}`,
            );
          }
        },
        onError: (error) => {
          if (error instanceof ApiError && error.status === 409)
            setConflicted(true);
        },
      },
    );
  };
  useEffect(() => {
    if (
      !dirty ||
      conflicted ||
      editorError ||
      unsupported ||
      readonly ||
      mutation.isPending
    )
      return;
    const timer = window.setTimeout(save, 1000);
    return () => window.clearTimeout(timer);
    // The timeout restarts on every local edit; it submits the exact draft seen by this render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    draft,
    baseId,
    dirty,
    conflicted,
    editorError,
    unsupported,
    readonly,
    mutation.isPending,
  ]);

  return (
    <Card className="panel-card">
      <h2>{t("generatedEditor.editTitle")}</h2>
      <p>
        {t("generatedEditor.editingVersion", { revision: revision.revision })}
      </p>
      <p>{t("generatedEditor.formatHint")}</p>
      {forkContext && <p>{t("generatedEditor.reuseDescription")}</p>}
      {readonly && <p>当前成员只可查看正文。</p>}
      <Field label="标题">
        <Input
          value={draft.title}
          disabled={
            readonly ||
            unsupported ||
            (Boolean(forkContext) && mutation.isPending)
          }
          onChange={(_, data) => update({ ...draft, title: data.value })}
        />
      </Field>
      <StructuredContentEditor
        key={editorEpoch}
        mediaScope={{ tenantId, projectId }}
        document={draft}
        baselineDocument={baselineDocument}
        readonly={
          readonly ||
          unsupported ||
          (Boolean(forkContext) && mutation.isPending) ||
          conflicted
        }
        onChange={(document, error) => {
          setEditorError(error);
          if (document)
            update((current) => ({ ...document, title: current.title }));
          else if (error) {
            changeGeneration.current++;
            setDirty(true);
            setSaved(false);
          }
        }}
      />
      <details className="content-technical-details">
        <summary>{t("generatedEditor.evidenceIdentifiers")}</summary>
        <p>
          {draft.blocks
            .map(
              (block) =>
                `${block.block_id} · ${block.citation_ids.join("、") || "无"}`,
            )
            .join("；")}
        </p>
      </details>
      {editorError && (
        <MessageBar intent="warning">
          <MessageBarBody>
            {editorError} 本地编辑仍保留，不会提交旧草稿。
          </MessageBarBody>
        </MessageBar>
      )}
      {!readonly && !unsupported && (
        <div>
          <Button
            appearance="primary"
            disabled={
              !dirty || mutation.isPending || conflicted || Boolean(editorError)
            }
            onClick={save}
          >
            {mutation.isPending
              ? "正在保存…"
              : forkContext
                ? "创建本轮修订"
                : "保存新版本"}
          </Button>
        </div>
      )}
      <p aria-live="polite">
        {conflicted
          ? "服务器版本已改变；本地输入仍保留。请复制或比较草稿后再刷新，当前不会覆盖它。"
          : editorError
            ? t("generatedEditor.unsavedInvalid")
            : mutation.isPending
              ? "正在保存新版本"
              : dirty
                ? "本地草稿未保存；停止输入 1 秒后自动保存。"
                : saved
                  ? "新版本已保存，自动检查尚未完成。"
                  : "无未保存更改"}
      </p>
      {mutation.isError && !conflicted && (
        <ErrorState
          title={apiTitle(mutation.error, "保存新版本失败")}
          detail={mutation.error.message}
          onRetry={save}
        />
      )}
      {conflicted && (
        <Button
          onClick={() => {
            setConflicted(false);
            setDirty(false);
            setSaved(false);
            setDraft(structuredClone(revision.document));
            setBaselineDocument(revision.document);
            setBaseId(revision.revision_id);
            setEditorError(null);
            setEditorEpoch((epoch) => epoch + 1);
          }}
        >
          放弃本地草稿并加载所选版本
        </Button>
      )}
    </Card>
  );
}

function AssetContent({
  tenantId,
  projectId,
  assetId,
}: {
  tenantId: string;
  projectId: string;
  assetId: string;
}) {
  const { t } = useTranslation();
  const [exporting, setExporting] = useState<"markdown" | "html" | null>(null);
  const [exportError, setExportError] = useState<string | null>(null);
  const { session } = useAuth();
  const readonly = !mayEdit(membershipForTenant(session, tenantId)?.role);
  const [searchParams] = useSearchParams();
  const reuseExecutionId = searchParams.get("reuse_execution_id");
  const reuseItemId = searchParams.get("reuse_item_id");
  const hasReuseContext = Boolean(reuseExecutionId || reuseItemId);
  const contextualItems = useContentItemsQuery(
    tenantId,
    projectId,
    reuseExecutionId ?? undefined,
  );
  const asset = useContentAssetQuery(tenantId, projectId, assetId);
  const history = useContentRevisionsQuery(tenantId, projectId, assetId);
  const [selectedId, setSelectedId] = useState<string>();
  const resolvedCurrent = history.data?.find(
    (revision) => revision.revision_id === asset.data?.current_revision_id,
  );
  // Asset and revision queries can briefly resolve in different orders after
  // a save; retain the last valid editor rather than remounting it mid-input.
  const lastCurrent = useRef<ContentRevision | undefined>(undefined);
  if (resolvedCurrent) lastCurrent.current = resolvedCurrent;
  const current = resolvedCurrent ?? lastCurrent.current;
  const selected =
    history.data?.find((revision) => revision.revision_id === selectedId) ??
    (hasReuseContext
      ? history.data?.find(
          (revision) =>
            revision.revision_id ===
            contextualItems.data?.find((item) => item.item_id === reuseItemId)
              ?.reuse_binding?.revision_id,
        )
      : undefined) ??
    current;
  const sorted = useMemo(
    () => [...(history.data ?? [])].sort((a, b) => b.revision - a.revision),
    [history.data],
  );
  const contextualItem = contextualItems.data?.find(
    (item) =>
      item.item_id === reuseItemId && item.execution_id === reuseExecutionId,
  );
  const isReusedSource =
    contextualItem?.reuse_binding?.asset_id === assetId &&
    contextualItem.asset_id === assetId &&
    history.data?.some(
      (revision) =>
        revision.revision_id === contextualItem.reuse_binding?.revision_id,
    );
  const isForkedCurrent =
    contextualItem?.reuse_binding &&
    contextualItem.asset_id === assetId &&
    contextualItem.reuse_binding.asset_id !== assetId;
  const validContext = isReusedSource || isForkedCurrent;
  const selectedHasMedia =
    selected?.document.blocks.some(
      (block) => block.rich && containsMedia(block.rich.node),
    ) ?? false;
  const download = async (format: "markdown" | "html") => {
    if (!selected || exporting || selectedHasMedia) return;
    setExportError(null);
    setExporting(format);
    try {
      // The asset and revision IDs come from the selected persisted history,
      // including the original immutable revision in a reuse context.
      const result = await exportContentRevision(
        tenantId,
        projectId,
        selected.asset_id,
        selected.revision_id,
        format,
      );
      if (
        result.revision_id !== selected.revision_id ||
        result.format !== format
      )
        throw new Error(t("generatedEditor.exportError"));
      const objectUrl = URL.createObjectURL(
        new Blob([result.content], { type: result.media_type }),
      );
      const anchor = document.createElement("a");
      anchor.href = objectUrl;
      anchor.download = result.filename;
      document.body.append(anchor);
      anchor.click();
      anchor.remove();
      window.setTimeout(() => URL.revokeObjectURL(objectUrl), 0);
    } catch {
      setExportError(t("generatedEditor.exportError"));
    } finally {
      setExporting(null);
    }
  };

  if (
    asset.isPending ||
    history.isPending ||
    (hasReuseContext && contextualItems.isPending)
  )
    return <LoadingState label="正在加载正文与版本历史" />;
  if (
    asset.isError ||
    history.isError ||
    (hasReuseContext && contextualItems.isError)
  ) {
    const error =
      asset.error ??
      history.error ??
      contextualItems.error ??
      new Error("内容资产不可用");
    return (
      <ErrorState
        title={apiTitle(error, "无法读取内容资产")}
        detail={error.message}
        onRetry={() =>
          void Promise.all([
            asset.refetch(),
            history.refetch(),
            ...(hasReuseContext ? [contextualItems.refetch()] : []),
          ])
        }
      />
    );
  }
  if (hasReuseContext && (!reuseExecutionId || !reuseItemId || !validContext))
    return (
      <ErrorState
        title="无法核对本轮复用项"
        detail="资产与本轮清单项或原版本不匹配；不会将编辑写入原资产。请返回本轮内容清单重新打开。"
      />
    );
  if (!asset.data || !history.data?.length || !current)
    return (
      <EmptyState
        title="没有可编辑的持久正文"
        detail="暂时无法读取正文，请刷新重试。"
        action={<Link to="../content">返回内容资产</Link>}
      />
    );
  return (
    <div className="workbench-page">
      <section className="page-hero">
        <div>
          <p className="eyebrow">内容编辑</p>
          <h1>{current.document.title}</h1>
          <p>
            {t("generatedEditor.currentVersion", {
              revision: current.revision,
            })}
          </p>
        </div>
        <Button
          onClick={() => void Promise.all([asset.refetch(), history.refetch()])}
          disabled={asset.isFetching || history.isFetching}
        >
          刷新版本
        </Button>
      </section>
      <Link to="../content">← 返回内容资产</Link>
      {contextualItem?.reuse_binding && (
        <Card className="panel-card">
          <h2>复用来源与本轮覆盖</h2>
          <p>{t("generatedEditor.reuseSummary")}</p>
          <details className="content-technical-details">
            <summary>{t("generatedEditor.evidenceIdentifiers")}</summary>
            <p>
              {contextualItem.execution_id} · {contextualItem.item_id} ·{" "}
              {contextualItem.reuse_binding.origin_execution_id} ·{" "}
              {contextualItem.reuse_binding.origin_item_id} ·{" "}
              {contextualItem.reuse_binding.check_id}
            </p>
          </details>
          <Link
            to={`../content/${encodeURIComponent(contextualItem.reuse_binding.asset_id)}`}
          >
            打开原资产（直接编辑将修改原资产）
          </Link>
        </Card>
      )}
      {!hasReuseContext && current.derived_from_revision_id && (
        <details className="content-technical-details">
          <summary>{t("generatedEditor.originalVersion")}</summary>
          <p>{current.derived_from_revision_id}</p>
        </details>
      )}
      {selected && (
        <>
          {(selected.revision_id === current.revision_id && !isReusedSource) ||
          (isReusedSource &&
            selected.revision_id ===
              contextualItem?.reuse_binding?.revision_id) ? (
            <RevisionEditor
              key={
                isReusedSource
                  ? `${assetId}/reuse/${selected.revision_id}`
                  : `${assetId}/current`
              }
              revision={selected}
              tenantId={tenantId}
              projectId={projectId}
              assetId={assetId}
              readonly={readonly}
              forkContext={
                isReusedSource &&
                selected.revision_id ===
                  contextualItem?.reuse_binding?.revision_id &&
                reuseExecutionId &&
                reuseItemId
                  ? { executionId: reuseExecutionId, itemId: reuseItemId }
                  : undefined
              }
            />
          ) : (
            <Card className="panel-card">
              <h2>历史版本 v{selected.revision}</h2>
              {selectedHasMedia ? (
                <StructuredContentEditor
                  key={selected.revision_id}
                  document={selected.document}
                  readonly
                  mediaScope={{ tenantId, projectId }}
                  onChange={() => {}}
                />
              ) : (
                <pre style={{ whiteSpace: "pre-wrap" }}>
                  {selected.markdown}
                </pre>
              )}
              <p>历史版本只读。返回当前版本才能追加修订。</p>
            </Card>
          )}
          <Card className="panel-card">
            <details className="content-history">
              <summary>{t("generatedEditor.history")}</summary>
              <ul>
                {sorted.map((revision) => (
                  <li key={revision.revision_id}>
                    <Button
                      appearance={
                        selected.revision_id === revision.revision_id
                          ? "primary"
                          : "subtle"
                      }
                      onClick={() =>
                        setSelectedId(
                          !hasReuseContext &&
                            revision.revision_id === current.revision_id
                            ? undefined
                            : revision.revision_id,
                        )
                      }
                    >
                      v{revision.revision} · {formatUiDate(revision.created_at)}
                      {revision.revision_id === current.revision_id
                        ? t("generatedEditor.current")
                        : ""}
                    </Button>
                  </li>
                ))}
              </ul>
              <div className="content-export-actions">
                {selectedHasMedia ? (
                  <p>{t("generatedEditor.mediaExportUnavailable")}</p>
                ) : (
                  <>
                    <Button
                      disabled={!!exporting}
                      onClick={() => void download("markdown")}
                    >
                      {t("generatedEditor.exportMarkdown")}
                    </Button>
                    <Button
                      disabled={!!exporting}
                      onClick={() => void download("html")}
                    >
                      {t("generatedEditor.exportHtml")}
                    </Button>
                  </>
                )}
              </div>
              {exportError && (
                <MessageBar intent="error">
                  <MessageBarBody>{exportError}</MessageBarBody>
                </MessageBar>
              )}
            </details>
          </Card>
          <Card className="panel-card">
            <h2>知识证据与原文</h2>
            {selected.quotes.length ? (
              <ol className="content-evidence-quotes">
                {selected.quotes.map((quote) => (
                  <li
                    key={`${quote.reference.source_version_id}/${quote.reference.chunk_id}`}
                  >
                    <blockquote>{quote.exact_quote}</blockquote>
                    <details className="content-technical-details">
                      <summary>{t("generatedEditor.sourceDetails")}</summary>
                      <p>
                        {quote.reference.source_version_id} ·{" "}
                        {quote.reference.chunk_id ?? "未定位"} ·{" "}
                        {locatorText(quote.reference.locator)}
                      </p>
                    </details>
                  </li>
                ))}
              </ol>
            ) : (
              <p>{t("generatedEditor.noQuotes")}</p>
            )}
          </Card>
          <Card className="panel-card">
            <h2>自动检查发现</h2>
            {selected.findings.length ? (
              <ul>
                {selected.findings.map((finding) => (
                  <li key={finding.finding_id}>
                    <strong>{finding.blocking ? "阻断" : "提示"}</strong>
                    <p>{finding.detail}</p>
                    <details className="content-technical-details">
                      <summary>
                        {t("generatedEditor.evidenceIdentifiers")}
                      </summary>
                      <p>
                        {finding.code} · {finding.block_id ?? "整篇"} ·{" "}
                        {finding.evidence
                          .map((ref) => ref.chunk_id)
                          .join("、") || "无"}
                      </p>
                    </details>
                  </li>
                ))}
              </ul>
            ) : (
              <p>暂无检查结果。</p>
            )}
          </Card>
        </>
      )}
    </div>
  );
}

export function ContentAssetPage() {
  const { tenantId, projectId, id } = useParams();
  if (!tenantId || !projectId || !id)
    return <ErrorState title="缺少项目或内容标识" />;
  return (
    <AssetContent
      key={`${tenantId}/${projectId}/${id}`}
      tenantId={tenantId}
      projectId={projectId}
      assetId={id}
    />
  );
}
