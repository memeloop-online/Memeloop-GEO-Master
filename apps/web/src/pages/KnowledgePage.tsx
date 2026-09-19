import { useMemo, useRef, useState } from "react";
import {
  Badge,
  Button,
  Card,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
  Select,
  Spinner,
  Tab,
  TabList,
  Textarea,
} from "@fluentui/react-components";
import {
  AddRegular,
  ArrowClockwiseRegular,
  ArrowUploadRegular,
  DismissRegular,
  OpenRegular,
  SearchRegular,
} from "@fluentui/react-icons";
import { useNavigate, useParams, useSearchParams } from "react-router-dom";
import {
  type CapabilityName,
  type FileUploadProgress,
  type ImportItem,
  type KnowledgeCapabilities,
  type KnowledgePurpose,
  type SourceKind,
  useFactsQuery,
  useImportKnowledgeMutation,
  useKnowledgeCapabilitiesQuery,
  useKnowledgeReleaseQuery,
  useProductsQuery,
  useSourcesQuery,
  useUploadFilesMutation,
} from "../api/knowledge";
import { ErrorState, EmptyState, LoadingState } from "../components/AsyncState";
import { StatusPill, type StatusKind } from "../components/StatusPill";

const capabilityLabels: Record<CapabilityName, string> = {
  ocr: "OCR 扫描识别",
  vector: "向量检索",
  llm: "LLM 问答",
  url_fetch: "网页抓取",
};

type ImportItemState = {
  id: string;
  label: string;
  state: "waiting" | "submitting" | "accepted" | "failed";
  detail?: string;
};

type FileItem = {
  id: string;
  file: File;
};

function factValue(value: unknown) {
  if (value === null || value === undefined) return "—";
  if (typeof value === "object") return JSON.stringify(value);
  return String(value);
}

function sourceKindLabel(kind: SourceKind) {
  const labels: Record<SourceKind, string> = {
    file: "文件",
    url: "网页",
    text: "文本",
    object: "已有对象",
    knowledge_collection: "知识集合",
    manual: "手工资料",
  };
  return labels[kind];
}

function importStatusLabel(state: ImportItemState["state"]) {
  const labels: Record<ImportItemState["state"], string> = {
    waiting: "待提交",
    submitting: "正在提交",
    accepted: "已受理",
    failed: "失败",
  };
  return labels[state];
}

function formatBytes(value: number) {
  if (value < 1024 * 1024) return `${Math.round(value / 1024)} KB`;
  return `${(value / 1024 / 1024).toFixed(0)} MB`;
}

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
    case "partial":
    case "unknown":
      return "uncertain";
    default:
      return "uncertain";
  }
}

export function KnowledgeCapabilitiesNotice({
  capabilities,
}: {
  capabilities: KnowledgeCapabilities | undefined;
}) {
  if (!capabilities) return null;
  const missing = (Object.keys(capabilityLabels) as CapabilityName[]).filter(
    (name) => !capabilities[name].available,
  );
  if (missing.length === 0) return null;
  return (
    <MessageBar intent="warning" className="knowledge-capability-notice">
      <MessageBarBody>
        <b>部分知识能力尚未配置</b>
        <span>
          {missing
            .map((name) => {
              const detail = capabilities[name].reason;
              return `${capabilityLabels[name]}${detail ? `：${detail}` : ""}`;
            })
            .join("；")}
          。系统不会以空结果代替未运行的处理。
        </span>
      </MessageBarBody>
    </MessageBar>
  );
}

function ImportSidebar({
  tenantId,
  projectId,
  onClose,
}: {
  tenantId: string | undefined;
  projectId: string | undefined;
  onClose: () => void;
}) {
  const fileInput = useRef<HTMLInputElement>(null);
  const [purpose, setPurpose] = useState<KnowledgePurpose>("public");
  const [files, setFiles] = useState<FileItem[]>([]);
  const [urls, setUrls] = useState("");
  const [text, setText] = useState("");
  const [objectRef, setObjectRef] = useState("");
  const [collectionRef, setCollectionRef] = useState("");
  const [states, setStates] = useState<Record<string, ImportItemState>>({});
  const clientItemIds = useRef(new Map<string, string>());
  const uploadFiles = useUploadFilesMutation(tenantId, projectId);
  const importBatch = useImportKnowledgeMutation(tenantId, projectId);
  const capabilities = useKnowledgeCapabilitiesQuery(tenantId, projectId);
  const maxUploadBytes =
    capabilities.data?.max_upload_bytes ?? 100 * 1024 * 1024;
  const maxBatchFiles = capabilities.data?.max_batch_files ?? 100;
  const parsableMediaTypes = capabilities.data?.supported_media_types ?? [];
  const acceptedMediaTypes = [
    ...parsableMediaTypes,
    ...(capabilities.data?.accepted_unparsed_media_types ?? []),
  ];

  const nonFileItems = useMemo(() => {
    const next: Array<ImportItem & { label: string }> = [];
    const clientItemId = (kind: string, value: string) => {
      const key = `${kind}:${value}`;
      const previous = clientItemIds.current.get(key);
      if (previous) return previous;
      const created = crypto.randomUUID();
      clientItemIds.current.set(key, created);
      return created;
    };
    for (const url of urls
      .split(/\r?\n/)
      .map((value) => value.trim())
      .filter(Boolean)) {
      next.push({
        client_item_id: clientItemId("url", url),
        kind: "url",
        name: url,
        url,
        purpose,
        label: url,
      });
    }
    if (text.trim()) {
      next.push({
        client_item_id: clientItemId("text", text.trim()),
        kind: "text",
        name: "粘贴文本",
        text: text.trim(),
        purpose,
        label: "粘贴文本",
      });
    }
    if (objectRef.trim()) {
      next.push({
        client_item_id: clientItemId("object", objectRef.trim()),
        kind: "object",
        name: objectRef.trim(),
        object_id: objectRef.trim(),
        purpose,
        label: `对象：${objectRef.trim()}`,
      });
    }
    if (collectionRef.trim()) {
      next.push({
        client_item_id: clientItemId(
          "knowledge_collection",
          collectionRef.trim(),
        ),
        kind: "knowledge_collection",
        name: collectionRef.trim(),
        knowledge_release_id: collectionRef.trim(),
        purpose,
        label: `集合：${collectionRef.trim()}`,
      });
    }
    return next;
  }, [collectionRef, objectRef, purpose, text, urls]);

  const busy = uploadFiles.isPending || importBatch.isPending;
  const settled = Object.values(states);
  const failedCount = settled.filter((item) => item.state === "failed").length;
  const acceptedCount = settled.filter(
    (item) => item.state === "accepted",
  ).length;

  function updateState(id: string, next: Omit<ImportItemState, "id">) {
    setStates((previous) => ({ ...previous, [id]: { id, ...next } }));
  }

  function addFiles(nextFiles: FileList | null) {
    if (!nextFiles) return;
    const additions = Array.from(nextFiles).map((file) => ({
      id: crypto.randomUUID(),
      file,
    }));
    setFiles((previous) => [...previous, ...additions].slice(0, maxBatchFiles));
  }

  async function submit() {
    if (!files.length && !nonFileItems.length) return;
    const fileIds = new Map(files.map((item) => [item.file, item.id]));
    for (const item of files) {
      updateState(item.id, { label: item.file.name, state: "waiting" });
    }
    for (const item of nonFileItems) {
      updateState(item.client_item_id, {
        label: item.label,
        state: "submitting",
      });
    }

    const filePromise = files.length
      ? uploadFiles
          .mutateAsync({
            files: files.map((item) => item.file),
            purpose,
            onProgress: (progress: FileUploadProgress) => {
              const id = fileIds.get(progress.file);
              if (!id) return;
              const state =
                progress.state === "accepted"
                  ? "accepted"
                  : progress.state === "failed"
                    ? "failed"
                    : "submitting";
              updateState(id, {
                label: progress.file.name,
                state,
                detail:
                  progress.error?.message ??
                  (progress.state === "accepted"
                    ? progress.result?.status === "succeeded"
                      ? "资料处理已完成"
                      : "已受理；处理状态将继续更新"
                    : undefined),
              });
            },
          })
          .then(() => undefined)
      : Promise.resolve();
    const importPromise = nonFileItems.length
      ? importBatch
          .mutateAsync({
            items: nonFileItems.map(({ label: _label, ...item }) => item),
          })
          .then((result) => {
            for (const item of result.items) {
              const original = nonFileItems.find(
                (candidate) => candidate.client_item_id === item.client_item_id,
              );
              updateState(item.client_item_id, {
                label: original?.label ?? item.client_item_id,
                state:
                  item.status === "queued" ||
                  item.status === "running" ||
                  item.status === "partial" ||
                  item.status === "succeeded"
                    ? "accepted"
                    : "failed",
                detail:
                  item.status === "partial"
                    ? "资料已受理，但只有部分单元完成处理"
                    : (item.error?.message ?? item.error?.reason),
              });
            }
          })
          .catch((error) => {
            for (const item of nonFileItems) {
              updateState(item.client_item_id, {
                label: item.label,
                state: "failed",
                detail:
                  error instanceof Error ? error.message : "导入请求失败。",
              });
            }
          })
      : Promise.resolve();
    await Promise.all([filePromise, importPromise]);
  }

  return (
    <aside className="knowledge-import-sidebar" aria-label="导入资料">
      <div className="knowledge-sidebar-heading">
        <div>
          <p className="eyebrow">资料中心</p>
          <h2>导入资料</h2>
          <p>每项独立受理；单项失败不会撤销已经受理的资料。</p>
        </div>
        <Button
          appearance="subtle"
          icon={<DismissRegular />}
          aria-label="关闭导入资料"
          onClick={onClose}
        />
      </div>
      <div className="knowledge-import-form">
        <Field label="用途">
          <Select
            value={purpose}
            onChange={(_, data) => setPurpose(data.value as KnowledgePurpose)}
          >
            <option value="public">公开资料</option>
            <option value="internal">内部资料</option>
          </Select>
        </Field>
        <section className="knowledge-import-section">
          <div className="knowledge-inline-heading">
            <div>
              <b>文件</b>
              <small>
                PDF、DOCX、XLSX、CSV、Markdown 或 TXT，单文件最多{" "}
                {formatBytes(maxUploadBytes)}，每批最多 {maxBatchFiles} 个文件。
              </small>
            </div>
            <Button
              appearance="secondary"
              icon={<ArrowUploadRegular />}
              onClick={() => fileInput.current?.click()}
            >
              选择文件
            </Button>
            <input
              ref={fileInput}
              hidden
              aria-label="选择资料文件"
              type="file"
              multiple
              accept=".pdf,.docx,.xlsx,.csv,.md,.markdown,.txt"
              onChange={(event) => {
                addFiles(event.target.files);
                event.currentTarget.value = "";
              }}
            />
          </div>
          {files.length > 0 && (
            <ul className="knowledge-import-queue">
              {files.map((item) => (
                <li key={item.id}>
                  <span>{item.file.name}</span>
                  <Button
                    appearance="subtle"
                    size="small"
                    disabled={busy}
                    aria-label={`移除 ${item.file.name}`}
                    onClick={() =>
                      setFiles((previous) =>
                        previous.filter((entry) => entry.id !== item.id),
                      )
                    }
                  >
                    移除
                  </Button>
                </li>
              ))}
            </ul>
          )}
        </section>
        <MessageBar intent="info">
          <MessageBarBody>
            文件会先上传，再由服务端按当前适配器决定能否解析。
            {acceptedMediaTypes.length > 0
              ? ` 当前可接收：${acceptedMediaTypes.join("、")}。`
              : ""}
            {parsableMediaTypes.length > 0
              ? ` 当前已配置解析：${parsableMediaTypes.join("、")}。`
              : " 当前未报告可解析格式；上传被受理不表示已经解析。"}
          </MessageBarBody>
        </MessageBar>
        <Field
          label="多个网页 URL"
          hint="每行一个；网页抓取未配置时会明确返回能力缺失。"
        >
          <Textarea
            resize="vertical"
            value={urls}
            onChange={(_, data) => setUrls(data.value)}
            placeholder={
              "https://example.com/products\nhttps://example.com/faq"
            }
          />
        </Field>
        <Field label="粘贴文本">
          <Textarea
            resize="vertical"
            value={text}
            onChange={(_, data) => setText(data.value)}
            placeholder="粘贴 FAQ、产品资料或说明。较长文本应作为文件上传。"
          />
        </Field>
        <Field label="已有对象引用">
          <Input
            value={objectRef}
            onChange={(_, data) => setObjectRef(data.value)}
            placeholder="输入当前授权范围内的对象 ID"
          />
        </Field>
        <Field label="已有知识集合引用">
          <Input
            value={collectionRef}
            onChange={(_, data) => setCollectionRef(data.value)}
            placeholder="输入要冻结的知识集合 / release 引用"
          />
        </Field>
        <Button
          appearance="primary"
          disabled={busy}
          onClick={() => void submit()}
        >
          {busy ? "正在受理…" : "开始导入"}
        </Button>
        {settled.length > 0 && (
          <section className="knowledge-import-results" aria-live="polite">
            <b>
              已受理 {acceptedCount} 项，失败 {failedCount} 项
            </b>
            <ul>
              {settled.map((item) => (
                <li key={item.id}>
                  <StatusPill
                    status={statusKind(
                      item.state === "accepted"
                        ? "succeeded"
                        : item.state === "failed"
                          ? "failed"
                          : "processing",
                    )}
                  />
                  <span>{item.label}</span>
                  <small>
                    {importStatusLabel(item.state)}
                    {item.detail ? `：${item.detail}` : ""}
                  </small>
                </li>
              ))}
            </ul>
          </section>
        )}
      </div>
    </aside>
  );
}

export function KnowledgePage() {
  const { tenantId, projectId } = useParams();
  const navigate = useNavigate();
  const [searchParams, setSearchParams] = useSearchParams();
  const [importOpen, setImportOpen] = useState(false);
  const view = searchParams.get("view") === "facts" ? "facts" : "sources";
  const query = searchParams.get("q") ?? "";
  const productId = searchParams.get("product") ?? null;
  const capabilities = useKnowledgeCapabilitiesQuery(tenantId, projectId);
  const sources = useSourcesQuery(
    tenantId,
    projectId,
    view === "sources" ? query : "",
  );
  const products = useProductsQuery(tenantId, projectId);
  const facts = useFactsQuery(
    tenantId,
    projectId,
    productId,
    view === "facts" ? query : "",
  );
  const release = useKnowledgeReleaseQuery(tenantId, projectId);
  const activeProduct = products.data?.items.find(
    (product) => product.product_id === productId,
  );

  function setFilter(next: Record<string, string | null>) {
    const params = new URLSearchParams(searchParams);
    for (const [key, value] of Object.entries(next)) {
      if (value) params.set(key, value);
      else params.delete(key);
    }
    setSearchParams(params, { replace: true });
  }

  const sourceLoading = sources.isPending && !sources.data;
  const factLoading = facts.isPending && !facts.data;
  const visibleFacts = facts.data?.items ?? [];

  return (
    <div className="knowledge-page">
      <section className="page-hero">
        <div>
          <p className="eyebrow">P03 · 企业知识库</p>
          <h1>企业知识库</h1>
          <p>原始资料、可定位片段、产品事实与使用影响保留在同一项目范围内。</p>
        </div>
        <div className="knowledge-hero-actions">
          <Button
            appearance="primary"
            icon={<AddRegular />}
            onClick={() => setImportOpen(true)}
          >
            导入资料
          </Button>
          <Button
            appearance="secondary"
            icon={<SearchRegular />}
            onClick={() => navigate("ask")}
          >
            知识问答
          </Button>
        </div>
      </section>
      {capabilities.isError && (
        <ErrorState
          title="无法读取知识处理能力"
          detail="资料仍可查看；新导入是否需要 OCR、向量、网页抓取或 LLM 暂时未知。"
          intent="warning"
          onRetry={() => void capabilities.refetch()}
        />
      )}
      <KnowledgeCapabilitiesNotice capabilities={capabilities.data} />
      {release.data && (
        <MessageBar intent="info" className="knowledge-release-note">
          <MessageBarBody>
            当前知识版本 #{release.data.sequence}
            。后续问答和检索会绑定这个不可变版本。
            {release.data.coverage?.note
              ? ` ${release.data.coverage.note}`
              : ""}
          </MessageBarBody>
        </MessageBar>
      )}
      <section className="knowledge-toolbar">
        <TabList
          selectedValue={view}
          onTabSelect={(_, data) =>
            setFilter({ view: data.value as string, product: null, q: null })
          }
        >
          <Tab value="sources">资料中心</Tab>
          <Tab value="facts">产品与事实</Tab>
        </TabList>
        <Input
          aria-label={view === "sources" ? "搜索资料" : "搜索事实"}
          contentBefore={<SearchRegular />}
          value={query}
          placeholder={view === "sources" ? "搜索资料名称" : "搜索属性或值"}
          onChange={(_, data) => setFilter({ q: data.value || null })}
        />
      </section>
      <section className="knowledge-workbench">
        <Card className="knowledge-column knowledge-navigation">
          <h2>资料与产品</h2>
          <Button
            appearance={view === "sources" ? "primary" : "subtle"}
            onClick={() => setFilter({ view: "sources", product: null })}
          >
            全部资料
          </Button>
          <div className="knowledge-nav-group">
            <b>产品</b>
            {products.isPending && <Spinner size="tiny" label="正在加载产品" />}
            {products.data?.items.map((product) => (
              <Button
                key={product.product_id}
                appearance={
                  productId === product.product_id ? "primary" : "subtle"
                }
                onClick={() =>
                  setFilter({ view: "facts", product: product.product_id })
                }
              >
                {product.name}
                {product.model ? ` · ${product.model}` : ""}
              </Button>
            ))}
            {products.isError && (
              <Button
                appearance="subtle"
                onClick={() => void products.refetch()}
              >
                重新加载产品
              </Button>
            )}
          </div>
          <div className="knowledge-nav-group">
            <b>导入任务</b>
            <small>
              {sources.data?.items.filter(
                (source) => source.import_status === "succeeded",
              ).length ?? 0}
              {" 完成 / "}
              {sources.data?.items.filter(
                (source) =>
                  source.import_status === "running" ||
                  source.import_status === "queued" ||
                  source.import_status === "partial",
              ).length ?? 0}
              {" 处理中"}
            </small>
          </div>
        </Card>
        <Card className="knowledge-column knowledge-main">
          {view === "sources" ? (
            <>
              <div className="knowledge-panel-heading">
                <div>
                  <h2>资料中心</h2>
                  <p>查看用途、解析进度与提取结果。</p>
                </div>
                {sources.isFetching && sources.data && <small>正在更新</small>}
              </div>
              {sourceLoading ? (
                <LoadingState compact label="正在加载资料" />
              ) : sources.isError ? (
                <ErrorState
                  title="资料列表暂时无法加载"
                  onRetry={() => void sources.refetch()}
                />
              ) : sources.data?.items.length === 0 ? (
                <EmptyState
                  title="还没有资料"
                  detail="导入文件、网页、粘贴文本或已有对象后，系统会独立处理每项资料。"
                  action={
                    <Button
                      appearance="primary"
                      onClick={() => setImportOpen(true)}
                    >
                      导入第一份资料
                    </Button>
                  }
                />
              ) : (
                <div className="knowledge-table-scroll">
                  <table className="knowledge-table">
                    <thead>
                      <tr>
                        <th>名称</th>
                        <th>类型</th>
                        <th>用途</th>
                        <th>处理状态</th>
                        <th>片段 / 事实</th>
                        <th>最近同步</th>
                      </tr>
                    </thead>
                    <tbody>
                      {sources.data?.items.map((source) => (
                        <tr
                          key={source.source_id}
                          tabIndex={0}
                          onClick={() =>
                            navigate(`sources/${source.source_id}`)
                          }
                          onKeyDown={(event) => {
                            if (event.key === "Enter" || event.key === " ") {
                              event.preventDefault();
                              navigate(`sources/${source.source_id}`);
                            }
                          }}
                        >
                          <td>
                            <b>{source.name}</b>
                            {source.product_names?.length ? (
                              <small>{source.product_names.join("、")}</small>
                            ) : null}
                          </td>
                          <td>{sourceKindLabel(source.kind)}</td>
                          <td>
                            {source.purpose === "internal"
                              ? "内部资料"
                              : "公开资料"}
                          </td>
                          <td>
                            <StatusPill
                              status={statusKind(
                                source.import_status ?? source.state,
                              )}
                              text={source.import_status ?? source.state}
                            />
                          </td>
                          <td>
                            {source.chunk_count ?? "—"} /{" "}
                            {source.fact_count ?? "—"}
                          </td>
                          <td>
                            {source.last_sync_at
                              ? new Date(source.last_sync_at).toLocaleString()
                              : "—"}
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}
            </>
          ) : (
            <>
              <div className="knowledge-panel-heading">
                <div>
                  <h2>{activeProduct ? activeProduct.name : "产品与事实"}</h2>
                  <p>事实保留型号、市场、币种和来源；冲突不被自动覆盖。</p>
                </div>
                {facts.isFetching && facts.data && <small>正在更新</small>}
              </div>
              {factLoading ? (
                <LoadingState compact label="正在加载事实" />
              ) : facts.isError ? (
                <ErrorState
                  title="事实暂时无法加载"
                  onRetry={() => void facts.refetch()}
                />
              ) : visibleFacts.length === 0 ? (
                <EmptyState
                  title={
                    activeProduct ? "此产品尚未提取事实" : "还没有可用事实"
                  }
                  detail="资料解析完成后，产品属性、价格和场景会保留其准确来源与适用范围。"
                  action={
                    <Button
                      appearance="primary"
                      onClick={() => setImportOpen(true)}
                    >
                      导入资料
                    </Button>
                  }
                />
              ) : (
                <div className="knowledge-table-scroll">
                  <table className="knowledge-table">
                    <thead>
                      <tr>
                        <th>属性</th>
                        <th>当前值</th>
                        <th>型号 / 市场</th>
                        <th>状态</th>
                      </tr>
                    </thead>
                    <tbody>
                      {visibleFacts.map((fact) => (
                        <tr key={fact.fact_id}>
                          <td>
                            <b>{fact.attribute}</b>
                            {fact.pinned && <small>客户固定</small>}
                          </td>
                          <td>
                            {factValue(fact.typed_value)}
                            {fact.unit ? ` ${fact.unit}` : ""}
                          </td>
                          <td>
                            {[fact.model, fact.market, fact.currency]
                              .filter(Boolean)
                              .join(" · ") || "—"}
                          </td>
                          <td>
                            <StatusPill
                              status={statusKind(fact.status)}
                              text={fact.status}
                            />
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}
            </>
          )}
        </Card>
        <Card className="knowledge-column knowledge-context">
          <h2>来源与影响</h2>
          {view === "sources" ? (
            <>
              <p>打开一份资料可查看原文、版本、定位片段和受影响的内容。</p>
              <Button
                appearance="secondary"
                icon={<OpenRegular />}
                disabled={!sources.data?.items[0]}
                onClick={() => {
                  const source = sources.data?.items[0];
                  if (source) navigate(`sources/${source.source_id}`);
                }}
              >
                打开最近资料
              </Button>
            </>
          ) : (
            <>
              <p>选择事实后可沿证据引用回到资料详情；内部资料会被标记用途。</p>
              {visibleFacts.slice(0, 3).map((fact) => (
                <div className="knowledge-evidence-chip" key={fact.fact_id}>
                  <Badge appearance="tint">
                    {fact.evidence_refs.length} 条来源
                  </Badge>
                  <span>{fact.attribute}</span>
                </div>
              ))}
            </>
          )}
          <div className="knowledge-context-footer">
            <Button
              appearance="subtle"
              icon={<ArrowClockwiseRegular />}
              onClick={() => {
                void sources.refetch();
                void facts.refetch();
              }}
            >
              刷新数据
            </Button>
          </div>
        </Card>
      </section>
      {importOpen && (
        <ImportSidebar
          tenantId={tenantId}
          projectId={projectId}
          onClose={() => setImportOpen(false)}
        />
      )}
    </div>
  );
}
