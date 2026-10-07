import { useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
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
import {
  Link,
  useNavigate,
  useParams,
  useSearchParams,
} from "react-router-dom";
import {
  type CapabilityName,
  type FileUploadProgress,
  type ImportItem,
  type ImportStatus,
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
import i18n from "../i18n";
import "../i18n/knowledgePage";

const capabilityNames: CapabilityName[] = [
  "pdf_parser",
  "docx_parser",
  "xlsx_parser",
  "ocr",
  "vector",
  "llm",
  "url_fetch",
];

type ImportItemState = {
  id: string;
  label: string;
  state: "waiting" | "submitting" | "accepted" | "failed";
  detail?: string;
  sourceId?: string;
  importStatus?: ImportStatus;
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
  return i18n.t(`sourceKind.${kind}`, { ns: "knowledgePage" });
}

function importStatusLabel(state: ImportItemState["state"]) {
  return i18n.t(`importPanel.state.${state}`, { ns: "knowledgePage" });
}

function processingLabel(status: ImportStatus | null | undefined) {
  return i18n.t(
    `importPanel.state.${status === "failed" ? "parseFailed" : (status ?? "unknown")}`,
    { ns: "knowledgePage" },
  );
}

function sourceStatusLabel(status: string | null | undefined) {
  const known = [
    "queued",
    "running",
    "partial",
    "succeeded",
    "failed",
    "cancelled",
    "available",
    "ready",
    "complete",
    "completed",
    "blocked",
    "removed",
    "conflicted",
    "superseded",
    "active",
    "processing",
    "unknown",
  ];
  return i18n.t(
    `sourceState.${known.includes(status ?? "") ? status : "unknown"}`,
    {
      ns: "knowledgePage",
    },
  );
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
  const { t } = useTranslation("knowledgePage");
  if (!capabilities) return null;
  const missing = capabilityNames.filter(
    (name) => !capabilities[name].available,
  );
  if (missing.length === 0) return null;
  return (
    <MessageBar intent="warning" className="knowledge-capability-notice">
      <MessageBarBody>
        <b>{t("missingTitle")}</b>
        <span>
          {t("unavailable", {
            names: missing
              .map((name) => t(`capability.${name}`))
              .join(t("separator")),
          })}
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
  const { t } = useTranslation("knowledgePage");
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
  const sourceItems = useSourcesQuery(tenantId, projectId).data?.items ?? [];
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
        name: t("importPanel.pastedText"),
        text: text.trim(),
        purpose,
        label: t("importPanel.pastedText"),
      });
    }
    if (objectRef.trim()) {
      next.push({
        client_item_id: clientItemId("object", objectRef.trim()),
        kind: "object",
        name: objectRef.trim(),
        object_id: objectRef.trim(),
        purpose,
        label: t("importPanel.objectLabel", { name: objectRef.trim() }),
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
        label: t("importPanel.collectionLabel", { name: collectionRef.trim() }),
      });
    }
    return next;
  }, [collectionRef, objectRef, purpose, t, text, urls]);

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
                    ? processingLabel(progress.result?.status)
                    : undefined),
                sourceId: progress.result?.source?.source_id,
                importStatus: progress.result?.status,
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
                    ? t("importPanel.partial")
                    : (item.error?.message ?? item.error?.reason),
                sourceId: item.source?.source_id,
                importStatus: item.status,
              });
            }
          })
          .catch((error) => {
            for (const item of nonFileItems) {
              updateState(item.client_item_id, {
                label: item.label,
                state: "failed",
                detail:
                  error instanceof Error
                    ? error.message
                    : t("importPanel.failed"),
              });
            }
          })
      : Promise.resolve();
    await Promise.all([filePromise, importPromise]);
  }

  return (
    <aside
      className="knowledge-import-sidebar"
      aria-label={t("importPanel.title")}
    >
      <div className="knowledge-sidebar-heading">
        <div>
          <p className="eyebrow">{t("sources")}</p>
          <h2>{t("importPanel.title")}</h2>
          <p>{t("importPanel.description")}</p>
        </div>
        <Button
          appearance="subtle"
          icon={<DismissRegular />}
          aria-label={t("importPanel.close")}
          onClick={onClose}
        />
      </div>
      <div className="knowledge-import-form">
        <Field label={t("purpose")}>
          <Select
            value={purpose}
            onChange={(_, data) => setPurpose(data.value as KnowledgePurpose)}
          >
            <option value="public">{t("public")}</option>
            <option value="internal">{t("internal")}</option>
          </Select>
        </Field>
        <section className="knowledge-import-section">
          <div className="knowledge-inline-heading">
            <div>
              <b>{t("importPanel.files")}</b>
              <small>
                {t("importPanel.fileLimits", {
                  size: formatBytes(maxUploadBytes),
                  count: maxBatchFiles,
                })}
              </small>
            </div>
            <Button
              appearance="secondary"
              icon={<ArrowUploadRegular />}
              onClick={() => fileInput.current?.click()}
            >
              {t("importPanel.choose")}
            </Button>
            <input
              ref={fileInput}
              hidden
              aria-label={t("importPanel.chooseLabel")}
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
                    aria-label={t("importPanel.removeFile", {
                      name: item.file.name,
                    })}
                    onClick={() =>
                      setFiles((previous) =>
                        previous.filter((entry) => entry.id !== item.id),
                      )
                    }
                  >
                    {t("importPanel.remove")}
                  </Button>
                </li>
              ))}
            </ul>
          )}
        </section>
        <MessageBar intent="info">
          <MessageBarBody>
            {t("importPanel.fileHelp")}{" "}
            {capabilities.data && !capabilities.data.pdf_parser.available
              ? ` ${t("importPanel.pdfUnavailable")}`
              : capabilities.data && !capabilities.data.ocr.available
                ? ` ${t("importPanel.pdfTextOnly")}`
                : ""}
            {capabilities.data && !capabilities.data.docx_parser.available
              ? ` ${t("importPanel.docxUnavailable")}`
              : ""}
            {capabilities.data && !capabilities.data.xlsx_parser.available
              ? ` ${t("importPanel.xlsxUnavailable")}`
              : ""}
            {acceptedMediaTypes.length > 0
              ? ` ${t("importPanel.acceptedTypes", { types: acceptedMediaTypes.join(t("separator")) })}`
              : ""}
            {parsableMediaTypes.length > 0
              ? ` ${t("importPanel.parsableTypes", { types: parsableMediaTypes.join(t("separator")) })}`
              : ` ${t("importPanel.noParsableTypes")}`}
          </MessageBarBody>
        </MessageBar>
        <Field label={t("importPanel.urls")} hint={t("importPanel.urlHint")}>
          <Textarea
            resize="vertical"
            value={urls}
            onChange={(_, data) => setUrls(data.value)}
            placeholder={
              "https://example.com/products\nhttps://example.com/faq"
            }
          />
        </Field>
        <Field label={t("importPanel.paste")}>
          <Textarea
            resize="vertical"
            value={text}
            onChange={(_, data) => setText(data.value)}
            placeholder={t("importPanel.pastePlaceholder")}
          />
        </Field>
        <Field label={t("importPanel.object")}>
          <Input
            value={objectRef}
            onChange={(_, data) => setObjectRef(data.value)}
            placeholder={t("importPanel.objectPlaceholder")}
          />
        </Field>
        <Field label={t("importPanel.collection")}>
          <Input
            value={collectionRef}
            onChange={(_, data) => setCollectionRef(data.value)}
            placeholder={t("importPanel.collectionPlaceholder")}
          />
        </Field>
        <Button
          appearance="primary"
          disabled={busy}
          onClick={() => void submit()}
        >
          {busy ? t("importPanel.submitting") : t("importPanel.submit")}
        </Button>
        {settled.length > 0 && (
          <section className="knowledge-import-results" aria-live="polite">
            <b>
              {t("importPanel.results", {
                accepted: acceptedCount,
                failed: failedCount,
              })}
            </b>
            <ul>
              {settled.map((item) => {
                const current = sourceItems.find(
                  (source) => source.source_id === item.sourceId,
                );
                const progress = current?.import_status ?? item.importStatus;
                return (
                  <li key={item.id}>
                    <StatusPill
                      status={statusKind(
                        item.state === "failed"
                          ? "failed"
                          : (progress ?? "queued"),
                      )}
                    />
                    <span>{item.label}</span>
                    <small>
                      {item.state === "accepted"
                        ? processingLabel(progress)
                        : importStatusLabel(item.state)}
                      {item.state !== "accepted" && item.detail
                        ? `${i18n.language === "en" ? ": " : "："}${item.detail}`
                        : ""}
                    </small>
                    {item.sourceId && tenantId && projectId && (
                      <Link
                        to={`/app/${encodeURIComponent(tenantId)}/${encodeURIComponent(projectId)}/knowledge/sources/${encodeURIComponent(item.sourceId)}`}
                      >
                        {t("importPanel.details")}
                      </Link>
                    )}
                  </li>
                );
              })}
            </ul>
          </section>
        )}
      </div>
    </aside>
  );
}

export function KnowledgePage() {
  const { t, i18n: locale } = useTranslation("knowledgePage");
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
          <p className="eyebrow">{t("eyebrow")}</p>
          <h1>{t("title")}</h1>
          <p>{t("description")}</p>
        </div>
        <div className="knowledge-hero-actions">
          <Button
            appearance="primary"
            icon={<AddRegular />}
            onClick={() => setImportOpen(true)}
          >
            {t("import")}
          </Button>
          <Button
            appearance="secondary"
            icon={<SearchRegular />}
            onClick={() => navigate("ask")}
          >
            {t("ask")}
          </Button>
        </div>
      </section>
      {capabilities.isError && (
        <ErrorState
          title={t("capabilitiesFailed")}
          detail={t("capabilitiesFailedDetail")}
          intent="warning"
          onRetry={() => void capabilities.refetch()}
        />
      )}
      <KnowledgeCapabilitiesNotice capabilities={capabilities.data} />
      {release.data && (
        <MessageBar intent="info" className="knowledge-release-note">
          <MessageBarBody>
            {t("release", { sequence: release.data.sequence })}
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
          <Tab value="sources">{t("sources")}</Tab>
          <Tab value="facts">{t("facts")}</Tab>
        </TabList>
        <Input
          aria-label={
            view === "sources" ? t("searchSources") : t("searchFacts")
          }
          contentBefore={<SearchRegular />}
          value={query}
          placeholder={
            view === "sources" ? t("searchSourceName") : t("searchAttribute")
          }
          onChange={(_, data) => setFilter({ q: data.value || null })}
        />
      </section>
      <section className="knowledge-workbench">
        <Card className="knowledge-column knowledge-navigation">
          <h2>{t("navTitle")}</h2>
          <Button
            appearance={view === "sources" ? "primary" : "subtle"}
            onClick={() => setFilter({ view: "sources", product: null })}
          >
            {t("allSources")}
          </Button>
          <div className="knowledge-nav-group">
            <b>{t("products")}</b>
            {products.isPending && (
              <Spinner size="tiny" label={t("loadingProducts")} />
            )}
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
                {t("retryProducts")}
              </Button>
            )}
          </div>
          <div className="knowledge-nav-group">
            <b>{t("tasks")}</b>
            <small>
              {t("taskCounts", {
                done:
                  sources.data?.items.filter(
                    (source) => source.import_status === "succeeded",
                  ).length ?? 0,
                active:
                  sources.data?.items.filter(
                    (source) =>
                      source.import_status === "running" ||
                      source.import_status === "queued",
                  ).length ?? 0,
                partial:
                  sources.data?.items.filter(
                    (source) => source.import_status === "partial",
                  ).length ?? 0,
                failed:
                  sources.data?.items.filter(
                    (source) => source.import_status === "failed",
                  ).length ?? 0,
              })}
            </small>
          </div>
        </Card>
        <Card className="knowledge-column knowledge-main">
          {view === "sources" ? (
            <>
              <div className="knowledge-panel-heading">
                <div>
                  <h2>{t("sources")}</h2>
                  <p>{t("sourcesDescription")}</p>
                </div>
                {sources.isFetching && sources.data && (
                  <small>{t("updating")}</small>
                )}
              </div>
              {sourceLoading ? (
                <LoadingState compact label={t("loadingSources")} />
              ) : sources.isError ? (
                <ErrorState
                  title={t("sourcesError")}
                  onRetry={() => void sources.refetch()}
                />
              ) : sources.data?.items.length === 0 ? (
                <EmptyState
                  title={t("noSources")}
                  detail={t("noSourcesDetail")}
                  action={
                    <Button
                      appearance="primary"
                      onClick={() => setImportOpen(true)}
                    >
                      {t("firstSource")}
                    </Button>
                  }
                />
              ) : (
                <div className="knowledge-table-scroll">
                  <table className="knowledge-table">
                    <thead>
                      <tr>
                        <th>{t("name")}</th>
                        <th>{t("type")}</th>
                        <th>{t("purpose")}</th>
                        <th>{t("processState")}</th>
                        <th>{t("chunksFacts")}</th>
                        <th>{t("lastSync")}</th>
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
                              <small>
                                {source.product_names.join(t("separator"))}
                              </small>
                            ) : null}
                          </td>
                          <td>{sourceKindLabel(source.kind)}</td>
                          <td>
                            {source.purpose === "internal"
                              ? t("internal")
                              : t("public")}
                          </td>
                          <td>
                            <StatusPill
                              status={statusKind(
                                source.import_status ?? source.state,
                              )}
                              text={sourceStatusLabel(
                                source.import_status ?? source.state,
                              )}
                            />
                          </td>
                          <td>
                            {source.chunk_count ?? "—"} /{" "}
                            {source.fact_count ?? "—"}
                          </td>
                          <td>
                            {source.last_sync_at
                              ? new Date(source.last_sync_at).toLocaleString(
                                  locale.language,
                                )
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
                  <h2>{activeProduct ? activeProduct.name : t("facts")}</h2>
                  <p>{t("factsDescription")}</p>
                </div>
                {facts.isFetching && facts.data && (
                  <small>{t("updating")}</small>
                )}
              </div>
              {factLoading ? (
                <LoadingState compact label={t("loadingFacts")} />
              ) : facts.isError ? (
                <ErrorState
                  title={t("factsError")}
                  onRetry={() => void facts.refetch()}
                />
              ) : visibleFacts.length === 0 ? (
                <EmptyState
                  title={activeProduct ? t("productNoFacts") : t("noFacts")}
                  detail={t("noFactsDetail")}
                  action={
                    <Button
                      appearance="primary"
                      onClick={() => setImportOpen(true)}
                    >
                      {t("import")}
                    </Button>
                  }
                />
              ) : (
                <div className="knowledge-table-scroll">
                  <table className="knowledge-table">
                    <thead>
                      <tr>
                        <th>{t("attribute")}</th>
                        <th>{t("currentValue")}</th>
                        <th>{t("modelMarket")}</th>
                        <th>{t("state")}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {visibleFacts.map((fact) => (
                        <tr key={fact.fact_id}>
                          <td>
                            <b>{fact.attribute}</b>
                            {fact.pinned && <small>{t("pinned")}</small>}
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
                              text={sourceStatusLabel(fact.status)}
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
          <h2>{t("context")}</h2>
          {view === "sources" ? (
            <>
              <p>{t("sourceContext")}</p>
              <Button
                appearance="secondary"
                icon={<OpenRegular />}
                disabled={!sources.data?.items[0]}
                onClick={() => {
                  const source = sources.data?.items[0];
                  if (source) navigate(`sources/${source.source_id}`);
                }}
              >
                {t("openRecent")}
              </Button>
            </>
          ) : (
            <>
              <p>{t("factContext")}</p>
              {visibleFacts.slice(0, 3).map((fact) => (
                <div className="knowledge-evidence-chip" key={fact.fact_id}>
                  <Badge appearance="tint">
                    {t("evidenceCount", { count: fact.evidence_refs.length })}
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
              {t("refresh")}
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
