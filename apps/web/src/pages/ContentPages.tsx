import { useEffect, useMemo, useState } from "react";
import {
  Badge,
  Button,
  Card,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
  Select,
  Textarea,
} from "@fluentui/react-components";
import {
  Link,
  useNavigate,
  useParams,
  useSearchParams,
} from "react-router-dom";
import { ApiError } from "../api/client";
import {
  type ContentBlock,
  type ContentItem,
  type ContentRevision,
  type StructuredDocument,
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
          <p>正文资产状态待读取；此项仍计入冻结清单分母。</p>
        ) : (
          <p>尚无持久正文资产；此项仍计入冻结清单分母。</p>
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
          <p className="eyebrow">P08 · 内容资产</p>
          <h1>本轮内容分支</h1>
          <p>
            周期 {cycleId} ·
            规划与正文执行分别记录；未生成正文的项也不会从清单消失。
          </p>
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
          detail="当前只有清单句柄，规划尚未保存；不能将其当作零篇正文。"
          action={<Link to="../campaigns/current">规划文档清单</Link>}
        />
      )}
      {plan && (
        <>
          <Card className="panel-card">
            <h2>冻结规划与正文进度</h2>
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
              <p>尚无正文执行；规划状态不能算作生成结果。</p>
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
                <p>
                  执行已持久化；恢复会重新派发未完成分支，不会覆盖就绪版本。
                </p>
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

function newId() {
  return globalThis.crypto.randomUUID();
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
  const navigate = useNavigate();
  const [draft, setDraft] = useState<StructuredDocument>(() =>
    structuredClone(revision.document),
  );
  const [baseId, setBaseId] = useState(revision.revision_id);
  const [dirty, setDirty] = useState(false);
  const [conflicted, setConflicted] = useState(false);
  const [saved, setSaved] = useState(false);
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
      setBaseId(revision.revision_id);
    }
    if (saved && revision.revision_id === baseId) setSaved(false);
  }, [baseId, dirty, revision, saved]);

  const update = (next: StructuredDocument) => {
    setDraft(next);
    setDirty(true);
    setSaved(false);
  };
  const save = () => {
    if (!dirty || mutation.isPending || conflicted) return;
    mutation.mutate(
      { baseRevisionId: baseId, document: draft },
      {
        onSuccess: (result) => {
          setBaseId(result.revision_id);
          setDirty(false);
          setSaved(true);
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
    if (!dirty || conflicted || readonly || mutation.isPending) return;
    const timer = window.setTimeout(save, 1000);
    return () => window.clearTimeout(timer);
    // The timeout restarts on every local edit; it submits the exact draft seen by this render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [draft, dirty, conflicted, readonly, mutation.isPending]);

  return (
    <Card className="panel-card">
      <h2>结构化正文草稿</h2>
      <p>
        当前基线 v{revision.revision} · {baseId}。每次保存追加不可变版本，
        不覆盖已发布内容；编辑后需要重新检查。
      </p>
      {forkContext && (
        <p>
          当前正文复用原资产。编辑会在本轮创建新资产和草稿，保留原版本及检查证据；新草稿须独立检查，不需要人工审批。
        </p>
      )}
      {readonly && <p>当前成员只可查看正文。</p>}
      <Field label="标题">
        <Input
          value={draft.title}
          disabled={readonly || mutation.isPending}
          onChange={(_, data) => update({ ...draft, title: data.value })}
        />
      </Field>
      {draft.blocks.map((block, index) => (
        <fieldset
          key={block.block_id}
          disabled={readonly || mutation.isPending}
        >
          <legend>内容块 {index + 1}</legend>
          <Field label={`类型 ${index + 1}`}>
            <Select
              value={block.kind}
              onChange={(event) =>
                update({
                  ...draft,
                  blocks: draft.blocks.map((entry) =>
                    entry.block_id === block.block_id
                      ? {
                          ...entry,
                          kind: event.target.value as ContentBlock["kind"],
                          items:
                            event.target.value === "list"
                              ? entry.items.length
                                ? entry.items
                                : [""]
                              : [],
                        }
                      : entry,
                  ),
                })
              }
            >
              <option value="heading">小标题</option>
              <option value="paragraph">段落</option>
              <option value="list">列表</option>
            </Select>
          </Field>
          <Field label={`正文 ${index + 1}`}>
            <Textarea
              value={block.text}
              onChange={(_, data) =>
                update({
                  ...draft,
                  blocks: draft.blocks.map((entry) =>
                    entry.block_id === block.block_id
                      ? { ...entry, text: data.value }
                      : entry,
                  ),
                })
              }
            />
          </Field>
          {block.kind === "list" && (
            <Field label={`列表项目 ${index + 1}（每行一项）`}>
              <Textarea
                value={block.items.join("\n")}
                onChange={(_, data) =>
                  update({
                    ...draft,
                    blocks: draft.blocks.map((entry) =>
                      entry.block_id === block.block_id
                        ? { ...entry, items: data.value.split("\n") }
                        : entry,
                    ),
                  })
                }
              />
            </Field>
          )}
          <p>引用片段 ID：{block.citation_ids.join("、") || "无"}</p>
        </fieldset>
      ))}
      {!readonly && (
        <div>
          <Button
            onClick={() =>
              update({
                ...draft,
                blocks: [
                  ...draft.blocks,
                  {
                    block_id: newId(),
                    kind: "paragraph",
                    text: "",
                    citation_ids: [],
                    items: [],
                  },
                ],
              })
            }
          >
            添加段落
          </Button>{" "}
          <Button
            appearance="primary"
            disabled={!dirty || mutation.isPending || conflicted}
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
            setBaseId(revision.revision_id);
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
  const current = history.data?.find(
    (revision) => revision.revision_id === asset.data?.current_revision_id,
  );
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
        detail="缺少当前版本或历史记录；不会从清单项编造正文。"
        action={<Link to="../content">返回内容资产</Link>}
      />
    );
  return (
    <div className="workbench-page">
      <section className="page-hero">
        <div>
          <p className="eyebrow">P09 · 内容编辑</p>
          <h1>{current.document.title}</h1>
          <p>
            资产 {assetId} · 当前持久版本 v{current.revision}
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
          <p>
            本轮执行 {contextualItem.execution_id} · 清单项{" "}
            {contextualItem.item_id}； 复用原执行{" "}
            {contextualItem.reuse_binding.origin_execution_id} · 原清单项{" "}
            {contextualItem.reuse_binding.origin_item_id} · 检查{" "}
            {contextualItem.reuse_binding.check_id}
            。本轮覆盖独立记录，原资产和检查证据保持不变。
          </p>
          <Link
            to={`../content/${encodeURIComponent(contextualItem.reuse_binding.asset_id)}`}
          >
            打开原资产（直接编辑将修改原资产）
          </Link>
        </Card>
      )}
      {!hasReuseContext && current.derived_from_revision_id && (
        <p>
          此版本从其他资产的版本 {current.derived_from_revision_id}{" "}
          派生；原资产与历史版本保持不变。
        </p>
      )}
      {selected && (
        <>
          <Card className="panel-card">
            <h2>版本历史</h2>
            <p>查看旧版本不会改写当前版本；修订和检查结果均由服务端记录。</p>
            <ul>
              {sorted.map((revision) => (
                <li key={revision.revision_id}>
                  <Button
                    appearance={
                      selected.revision_id === revision.revision_id
                        ? "primary"
                        : "subtle"
                    }
                    onClick={() => setSelectedId(revision.revision_id)}
                  >
                    v{revision.revision} · {revision.created_at}
                    {revision.revision_id === current.revision_id
                      ? "（当前）"
                      : ""}
                  </Button>
                </li>
              ))}
            </ul>
          </Card>
          {(selected.revision_id === current.revision_id && !isReusedSource) ||
          (isReusedSource &&
            selected.revision_id ===
              contextualItem?.reuse_binding?.revision_id) ? (
            <RevisionEditor
              key={`${assetId}/${selected.revision_id}`}
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
              <pre style={{ whiteSpace: "pre-wrap" }}>{selected.markdown}</pre>
              <p>历史版本只读。返回当前版本才能追加修订。</p>
            </Card>
          )}
          <Card className="panel-card">
            <h2>知识证据与原文</h2>
            {selected.quotes.length ? (
              <ol>
                {selected.quotes.map((quote) => (
                  <li
                    key={`${quote.reference.source_version_id}/${quote.reference.chunk_id}`}
                  >
                    <blockquote>{quote.exact_quote}</blockquote>
                    <p>
                      来源版本 {quote.reference.source_version_id} · 片段{" "}
                      {quote.reference.chunk_id ?? "未定位"} ·{" "}
                      {locatorText(quote.reference.locator)}
                    </p>
                  </li>
                ))}
              </ol>
            ) : (
              <p>此版本未提供可展示的原文摘录；不能声称有可核对的引用。</p>
            )}
          </Card>
          <Card className="panel-card">
            <h2>自动检查发现</h2>
            {selected.findings.length ? (
              <ul>
                {selected.findings.map((finding) => (
                  <li key={finding.finding_id}>
                    <strong>
                      {finding.blocking ? "阻断" : "提示"} · {finding.code}
                    </strong>
                    <p>{finding.detail}</p>
                    <p>
                      内容块 {finding.block_id ?? "整篇"} · 引用{" "}
                      {finding.evidence.map((ref) => ref.chunk_id).join("、") ||
                        "无"}
                    </p>
                  </li>
                ))}
              </ul>
            ) : (
              <p>
                无持久检查发现记录；这不代表所有事实、格式或渠道适配均已通过。
              </p>
            )}
          </Card>
        </>
      )}
      <MessageBar intent="info">
        <MessageBarBody>
          当前仅支持标题和类型化正文块修订；富文本、媒体、渠道预览及发布记录尚未接入本资产。
        </MessageBarBody>
      </MessageBar>
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
