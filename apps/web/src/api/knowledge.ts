import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useAuth } from "../auth/AuthProvider";
import { queryScopeFor, type QueryScope } from "../auth/types";
import {
  ApiError,
  apiFetch,
  createIdempotencyKey,
  type ApiRequestOptions,
} from "./client";

export type KnowledgePurpose = "public" | "internal";
export type SourceKind =
  "file" | "url" | "text" | "object" | "knowledge_collection" | "manual";
export type SourceState = "active" | "removed";
export type ImportStatus =
  "queued" | "running" | "partial" | "succeeded" | "failed" | "cancelled";
export type AnswerStatus = "answered" | "insufficient_evidence" | "conflicted";
export type LocatorKind =
  "pdf" | "docx" | "xlsx" | "web" | "text" | "csv" | "manual";

export interface Capability {
  available: boolean;
  reason?: string | null;
  configured_at?: string | null;
}

export type CapabilityName = "ocr" | "vector" | "llm" | "url_fetch";

export interface KnowledgeCapabilities {
  ocr: Capability;
  vector: Capability;
  llm: Capability;
  url_fetch: Capability;
  max_upload_bytes?: number | null;
  max_batch_files?: number | null;
  /** Formats whose parser is configured, not merely allowed through upload. */
  supported_media_types?: string[];
  accepted_unparsed_media_types?: string[];
  /** Compatibility alias used by an earlier capability draft. */
  parse_supported_media_types?: string[];
}

export interface SourceLocator {
  kind: LocatorKind | "unknown";
  page?: number | null;
  bbox?: number[] | null;
  coordinate_system?: string | null;
  ocr?: boolean | null;
  heading_path?: string[] | null;
  paragraph_index?: number | null;
  table?: { row?: number; column?: number } | null;
  sheet?: string | null;
  range?: string | null;
  header_range?: string | null;
  snapshot_object_id?: string | null;
  original_url?: string | null;
  selector?: string | null;
  line_start?: number | null;
  line_end?: number | null;
  char_start?: number | null;
  char_end?: number | null;
  /** Current API field names for text locators. */
  start_line?: number | null;
  end_line?: number | null;
  start_char?: number | null;
  end_char?: number | null;
  start_row?: number | null;
  end_row?: number | null;
  start_column?: number | null;
  end_column?: number | null;
}

export interface SourceSummary {
  source_id: string;
  revision: number;
  kind: SourceKind;
  name: string;
  purpose: KnowledgePurpose;
  state: SourceState;
  current_version_id?: string | null;
  product_ids: string[];
  product_names?: string[];
  import_status?: ImportStatus | null;
  chunk_count?: number | null;
  fact_count?: number | null;
  sync_enabled?: boolean | null;
  next_sync_at?: string | null;
  last_sync_at?: string | null;
  updated_at?: string | null;
}

export interface SourceVersion {
  source_version_id: string;
  source_id: string;
  version: number;
  content_sha256?: string | null;
  captured_at?: string | null;
  original_url?: string | null;
  parser_version?: string | null;
  extraction_version?: string | null;
  created_at?: string | null;
}

export interface SourceChunk {
  chunk_id: string;
  source_version_id: string;
  ordinal: number;
  kind: "paragraph" | "table" | "image_description";
  text: string;
  locator: SourceLocator;
  product_ids: string[];
  market?: string | null;
  language?: string | null;
  extraction_method?: string | null;
  confidence?: number | null;
}

export interface EvidenceReference {
  source_id: string;
  source_version_id?: string | null;
  chunk_id?: string | null;
  fact_id?: string | null;
  source_name?: string | null;
  purpose?: KnowledgePurpose | null;
  locator?: SourceLocator | null;
  label?: string | null;
  text?: string | null;
  quote?: string | null;
}

export interface Product {
  product_id: string;
  revision: number;
  name: string;
  model?: string | null;
  aliases: string[];
  state: string;
  evidence_refs: EvidenceReference[];
}

export interface Fact {
  fact_id: string;
  revision: number;
  subject_id?: string | null;
  product_id?: string | null;
  model?: string | null;
  attribute: string;
  typed_value: string | number | boolean | Record<string, unknown> | null;
  unit?: string | null;
  market?: string | null;
  language?: string | null;
  currency?: string | null;
  effective_from?: string | null;
  effective_to?: string | null;
  status: "available" | "conflicted" | "superseded" | "blocked" | string;
  pinned: boolean;
  evidence_refs: EvidenceReference[];
}

export interface ImportJob {
  import_job_id: string;
  operation_id?: string | null;
  source_id: string;
  source_version_id?: string | null;
  stage: "acquire" | "parse" | "extract" | "index" | "release" | string;
  status: ImportStatus;
  attempt?: number | null;
  completed_units?: number | null;
  failed_units?: number | null;
  errors?: Array<{ code?: string; message?: string; unit?: string }> | null;
  updated_at?: string | null;
}

export interface SourceImpact {
  document_manifest_items?: Array<{
    id: string;
    label: string;
    status: string;
  }>;
  content?: Array<{
    id: string;
    title: string;
    revision?: string | number;
    status?: string;
  }>;
  publications?: Array<{
    id: string;
    label: string;
    status: string;
  }>;
}

export interface SourceDetail {
  source: SourceSummary;
  versions: SourceVersion[];
  chunks: SourceChunk[];
  facts: Fact[];
  import_jobs: ImportJob[];
  impact: SourceImpact;
  original_text?: string | null;
}

export interface KnowledgeRelease {
  knowledge_release_id: string;
  sequence: number;
  created_at?: string | null;
  coverage?: {
    completed_sources?: number | null;
    partial_sources?: number | null;
    failed_sources?: number | null;
    note?: string | null;
  } | null;
}

export interface SourceListResponse {
  items: SourceSummary[];
  next_cursor: string | null;
}

export interface ProductListResponse {
  items: Product[];
  next_cursor: string | null;
}

export interface FactListResponse {
  items: Fact[];
  next_cursor: string | null;
}

export interface CreateUploadSessionInput {
  filename: string;
  declared_media_type: string;
  expected_size: number;
  expected_sha256: string;
  purpose: KnowledgePurpose;
}

export interface UploadSession {
  upload_session_id: string;
  revision?: number;
  filename: string;
  expected_size: number;
  purpose: KnowledgePurpose;
  state:
    | "created"
    | "uploading"
    | "uploaded"
    | "committed"
    | "failed"
    | "expired"
    | "cancelled";
  expires_at?: string | null;
}

/** Completion verifies the staged bytes; the current server command is `{}`. */
export type CompleteUploadInput = Record<never, never>;

export interface UploadCompletion {
  client_item_id?: string | null;
  status: ImportStatus;
  source?: SourceSummary | null;
  /** Compatibility for an early server spelling; remove after API migration. */
  souce?: SourceSummary | null;
  source_version?: SourceVersion | null;
  import_job?: ImportJob | null;
  operation?: { operation_id: string; status?: string } | null;
  release?: KnowledgeRelease | null;
  error?: { code?: string; message?: string; reason?: string } | null;
}

interface ImportItemBase {
  client_item_id: string;
  name: string;
  purpose: KnowledgePurpose;
}

export type ImportItem =
  | (ImportItemBase & {
      kind: "url";
      url: string;
    })
  | (ImportItemBase & {
      kind: "text";
      text: string;
    })
  | (ImportItemBase & {
      kind: "object";
      object_id: string;
    })
  | (ImportItemBase & {
      kind: "knowledge_collection";
      knowledge_release_id: string;
    });

export interface ImportBatchInput {
  items: ImportItem[];
}

export interface ImportItemResult {
  client_item_id: string;
  status: ImportStatus;
  source?: SourceSummary | null;
  /** Compatibility for an early server spelling; remove after API migration. */
  souce?: SourceSummary | null;
  source_version?: SourceVersion | null;
  import_job?: ImportJob | null;
  operation?: { operation_id: string; status?: string } | null;
  release?: KnowledgeRelease | null;
  error?: {
    code?: string;
    message?: string;
    retryable?: boolean;
    reason?: string;
  } | null;
}

export interface ImportBatchResult {
  items: ImportItemResult[];
}

export interface SearchInput {
  query: string;
  knowledge_release_id?: string | null;
  purpose: KnowledgePurpose;
  limit: number;
}

export interface SearchResult {
  evidence: EvidenceReference[];
  status?: AnswerStatus;
  missing?: string[];
}

export type AskInput = SearchInput;

export interface AskResult {
  status: AnswerStatus;
  mode: "answer" | "evidence_only";
  answer?: string | null;
  evidence: EvidenceReference[];
  missing: string[];
  conflicts?: string[];
  capability_missing?: string[] | null;
  knowledge_release_id?: string | null;
}

export interface FileUploadProgress {
  file: File;
  state:
    | "waiting"
    | "creating_session"
    | "uploading"
    | "completing"
    | "accepted"
    | "failed";
  result?: UploadCompletion;
  error?: Error;
}

function scopeKey(scope: QueryScope) {
  return [
    scope.userId,
    scope.operatorId,
    scope.tenantId,
    scope.projectId ?? null,
  ] as const;
}

export const knowledgeQueryKeys = {
  all: ["knowledge"] as const,
  capabilities: (scope: QueryScope) =>
    [...knowledgeQueryKeys.all, "capabilities", ...scopeKey(scope)] as const,
  sources: (scope: QueryScope, query: string) =>
    [...knowledgeQueryKeys.all, "sources", ...scopeKey(scope), query] as const,
  source: (scope: QueryScope, sourceId: string) =>
    [
      ...knowledgeQueryKeys.all,
      "source",
      ...scopeKey(scope),
      sourceId,
    ] as const,
  products: (scope: QueryScope) =>
    [...knowledgeQueryKeys.all, "products", ...scopeKey(scope)] as const,
  facts: (scope: QueryScope, productId: string | null, query: string) =>
    [
      ...knowledgeQueryKeys.all,
      "facts",
      ...scopeKey(scope),
      productId,
      query,
    ] as const,
  release: (scope: QueryScope) =>
    [...knowledgeQueryKeys.all, "release", ...scopeKey(scope)] as const,
};

function asRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === "object"
    ? (value as Record<string, unknown>)
    : {};
}

function asArray<T>(value: unknown): T[] {
  return Array.isArray(value) ? (value as T[]) : [];
}

function capability(value: unknown): Capability {
  if (typeof value === "boolean") return { available: value };
  const candidate = asRecord(value);
  return {
    available:
      candidate.available === true ||
      candidate.state === "available" ||
      candidate.enabled === true,
    reason:
      typeof candidate.reason === "string"
        ? candidate.reason
        : typeof candidate.message === "string"
          ? candidate.message
          : null,
    configured_at:
      typeof candidate.configured_at === "string"
        ? candidate.configured_at
        : null,
  };
}

function normalizeCapabilities(value: unknown): KnowledgeCapabilities {
  const root = asRecord(value);
  const values = asRecord(root.capabilities ?? root);
  return {
    ocr: capability(values.ocr),
    vector: capability(values.vector ?? values.vector_search),
    llm: capability(values.llm ?? values.llm_answering),
    url_fetch: capability(values.url_fetch ?? values.urlFetch),
    max_upload_bytes:
      typeof values.max_upload_bytes === "number"
        ? values.max_upload_bytes
        : typeof root.max_upload_bytes === "number"
          ? root.max_upload_bytes
          : null,
    max_batch_files:
      typeof values.max_batch_files === "number"
        ? values.max_batch_files
        : typeof root.max_batch_files === "number"
          ? root.max_batch_files
          : null,
    supported_media_types: asArray<string>(
      values.supported_media_types ?? root.supported_media_types,
    ),
    accepted_unparsed_media_types: asArray<string>(
      values.accepted_unparsed_media_types ??
        root.accepted_unparsed_media_types,
    ),
    parse_supported_media_types: asArray<string>(
      values.parse_supported_media_types ??
        values.parsable_media_types ??
        root.parse_supported_media_types ??
        values.supported_media_types ??
        root.supported_media_types,
    ),
  };
}

function normalizeCollection<T>(value: unknown): {
  items: T[];
  next_cursor: string | null;
} {
  if (Array.isArray(value)) return { items: value as T[], next_cursor: null };
  const root = asRecord(value);
  return {
    items: asArray<T>(root.items ?? root.data),
    next_cursor: typeof root.next_cursor === "string" ? root.next_cursor : null,
  };
}

function sourceSummary(value: SourceSummary): SourceSummary {
  return {
    ...value,
    product_ids: value.product_ids ?? [],
    product_names: value.product_names ?? [],
  };
}

function includesQuery(value: string, query: string) {
  return value.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase());
}

function valueText(value: Fact["typed_value"]) {
  if (value === null || value === undefined) return "";
  if (typeof value === "object") return JSON.stringify(value);
  return String(value);
}

function matchesSourceQuery(source: SourceSummary, query: string) {
  if (!query.trim()) return true;
  return includesQuery(
    [
      source.name,
      source.kind,
      source.purpose,
      ...source.product_ids,
      ...(source.product_names ?? []),
    ].join(" "),
    query,
  );
}

function matchesFactQuery(fact: Fact, productId: string | null, query: string) {
  if (productId && fact.product_id !== productId) return false;
  if (!query.trim()) return true;
  return includesQuery(
    [
      fact.subject_id,
      fact.product_id,
      fact.model,
      fact.attribute,
      valueText(fact.typed_value),
      fact.unit,
      fact.market,
      fact.language,
      fact.currency,
      fact.status,
    ]
      .filter((value): value is string => typeof value === "string")
      .join(" "),
    query,
  );
}

function normalizeSourceDetail(value: unknown): SourceDetail {
  const root = asRecord(value);
  const source = asRecord(root.source ?? root) as unknown as SourceSummary;
  return {
    source: sourceSummary(source),
    versions: asArray<SourceVersion>(root.versions),
    chunks: asArray<SourceChunk>(root.chunks),
    facts: asArray<Fact>(root.facts),
    import_jobs: asArray<ImportJob>(root.import_jobs ?? root.importJobs),
    impact: (root.impact ?? {}) as SourceImpact,
    original_text:
      typeof root.original_text === "string"
        ? root.original_text
        : typeof root.originalText === "string"
          ? root.originalText
          : null,
  };
}

function normalizeAskResult(value: unknown): AskResult {
  const root = asRecord(value);
  const rawStatus = root.answer_status ?? root.status;
  const status: AnswerStatus =
    rawStatus === "conflicted"
      ? "conflicted"
      : rawStatus === "insufficient_evidence" || rawStatus === "insufficient"
        ? "insufficient_evidence"
        : "answered";
  const evidence = asArray<EvidenceReference>(root.evidence).slice(0, 12);
  const mode =
    root.mode === "evidence_only" || root.answer_mode === "evidence_only"
      ? "evidence_only"
      : "answer";
  return {
    status,
    mode,
    answer: typeof root.answer === "string" ? root.answer : null,
    evidence,
    missing: asArray<string>(root.missing ?? root.gaps),
    conflicts: asArray<string>(root.conflicts),
    capability_missing: Array.isArray(root.capability_missing)
      ? asArray<string>(root.capability_missing)
      : root.capability_missing
        ? [String(root.capability_missing)]
        : null,
    knowledge_release_id:
      typeof root.knowledge_release_id === "string"
        ? root.knowledge_release_id
        : null,
  };
}

function normalizeImportAcceptance(value: unknown): ImportItemResult {
  const root = asRecord(value);
  const error = asRecord(root.error);
  return {
    client_item_id:
      typeof root.client_item_id === "string" ? root.client_item_id : "",
    status: (typeof root.status === "string"
      ? root.status
      : "failed") as ImportStatus,
    source: (root.source ?? root.souce ?? null) as SourceSummary | null,
    souce: root.souce as SourceSummary | null | undefined,
    source_version: (root.source_version ?? null) as SourceVersion | null,
    import_job: (root.import_job ?? null) as ImportJob | null,
    operation: (root.operation ?? null) as {
      operation_id: string;
      status?: string;
    } | null,
    release: (root.release ?? null) as KnowledgeRelease | null,
    error: Object.keys(error).length
      ? {
          code: typeof error.code === "string" ? error.code : undefined,
          message:
            typeof error.message === "string" ? error.message : undefined,
          retryable: error.retryable === true,
          reason: typeof error.reason === "string" ? error.reason : undefined,
        }
      : null,
  };
}

function normalizeImportBatch(value: unknown): ImportBatchResult {
  const root = asRecord(value);
  return {
    items: asArray<unknown>(root.items ?? root.acceptances).map(
      normalizeImportAcceptance,
    ),
  };
}

function scopeFor(
  session: ReturnType<typeof useAuth>["session"],
  tenantId: string | undefined,
  projectId: string | undefined,
) {
  return session && tenantId && projectId
    ? queryScopeFor(session, tenantId, projectId)
    : undefined;
}

function scopedOptions(tenantId: string, projectId: string): ApiRequestOptions {
  return { tenantId, projectId };
}

export async function getKnowledgeCapabilities(
  tenantId: string,
  projectId: string,
): Promise<KnowledgeCapabilities> {
  return normalizeCapabilities(
    await apiFetch<unknown>(
      "/knowledge/capabilities",
      scopedOptions(tenantId, projectId),
    ),
  );
}

export async function listSources(
  tenantId: string,
  projectId: string,
  query = "",
): Promise<SourceListResponse> {
  const response = normalizeCollection<SourceSummary>(
    await apiFetch<unknown>(
      "/knowledge/sources",
      scopedOptions(tenantId, projectId),
    ),
  );
  return {
    ...response,
    items: response.items
      .map(sourceSummary)
      .filter((source) => matchesSourceQuery(source, query)),
  };
}

export async function getSource(
  tenantId: string,
  projectId: string,
  sourceId: string,
): Promise<SourceDetail> {
  return normalizeSourceDetail(
    await apiFetch<unknown>(
      `/knowledge/sources/${encodeURIComponent(sourceId)}`,
      scopedOptions(tenantId, projectId),
    ),
  );
}

export async function listProducts(
  tenantId: string,
  projectId: string,
): Promise<ProductListResponse> {
  return normalizeCollection<Product>(
    await apiFetch<unknown>(
      "/knowledge/products",
      scopedOptions(tenantId, projectId),
    ),
  );
}

export async function listFacts(
  tenantId: string,
  projectId: string,
  productId: string | null,
  query = "",
): Promise<FactListResponse> {
  const response = normalizeCollection<Fact>(
    await apiFetch<unknown>(
      "/knowledge/facts",
      scopedOptions(tenantId, projectId),
    ),
  );
  return {
    ...response,
    items: response.items.filter((fact) =>
      matchesFactQuery(fact, productId, query),
    ),
  };
}

export async function getCurrentKnowledgeRelease(
  tenantId: string,
  projectId: string,
): Promise<KnowledgeRelease | null> {
  try {
    const current = await apiFetch<{
      knowledge_release_id?: string | null;
      sequence?: number | null;
    }>("/knowledge/releases/current", scopedOptions(tenantId, projectId));
    if (
      !current.knowledge_release_id ||
      current.sequence === null ||
      current.sequence === undefined
    ) {
      return null;
    }
    return {
      knowledge_release_id: current.knowledge_release_id,
      sequence: current.sequence,
    };
  } catch (error) {
    if (error instanceof ApiError && error.status === 404) return null;
    throw error;
  }
}

export function createUploadSession(
  tenantId: string,
  projectId: string,
  input: CreateUploadSessionInput,
  idempotencyKey = createIdempotencyKey(),
): Promise<UploadSession> {
  return apiFetch<UploadSession>("/knowledge/upload-sessions", {
    ...scopedOptions(tenantId, projectId),
    method: "POST",
    body: input,
    idempotencyKey,
  });
}

export function uploadSessionContent(
  tenantId: string,
  projectId: string,
  sessionId: string,
  content: File,
): Promise<void> {
  return apiFetch<void>(
    `/knowledge/upload-sessions/${encodeURIComponent(sessionId)}/content`,
    {
      ...scopedOptions(tenantId, projectId),
      method: "PUT",
      rawBody: content,
      idempotency: "omit",
    },
  );
}

export function completeUploadSession(
  tenantId: string,
  projectId: string,
  sessionId: string,
  input: CompleteUploadInput,
  idempotencyKey = createIdempotencyKey(),
): Promise<UploadCompletion> {
  return apiFetch<unknown>(
    `/knowledge/upload-sessions/${encodeURIComponent(sessionId)}/complete`,
    {
      ...scopedOptions(tenantId, projectId),
      method: "POST",
      body: input,
      idempotencyKey,
    },
  ).then((value) => normalizeImportAcceptance(value));
}

async function sha256(file: File) {
  if (!globalThis.crypto?.subtle) {
    throw new Error("浏览器不支持 SHA-256 核验，无法安全创建上传会话。");
  }
  const buffer = await file.arrayBuffer();
  const digest = await globalThis.crypto.subtle.digest("SHA-256", buffer);
  return Array.from(new Uint8Array(digest), (value) =>
    value.toString(16).padStart(2, "0"),
  ).join("");
}

function declaredMediaType(file: File) {
  if (file.type) return file.type;
  const filename = file.name.toLocaleLowerCase();
  if (filename.endsWith(".txt")) return "text/plain";
  if (filename.endsWith(".md") || filename.endsWith(".markdown")) {
    return "text/markdown";
  }
  if (filename.endsWith(".csv")) return "text/csv";
  return "application/octet-stream";
}

export async function uploadFile(
  tenantId: string,
  projectId: string,
  file: File,
  purpose: KnowledgePurpose,
  onProgress?: (state: FileUploadProgress) => void,
): Promise<UploadCompletion> {
  const waiting: FileUploadProgress = { file, state: "creating_session" };
  onProgress?.(waiting);
  try {
    const expectedSha256 = await sha256(file);
    const session = await createUploadSession(tenantId, projectId, {
      filename: file.name,
      declared_media_type: declaredMediaType(file),
      expected_size: file.size,
      expected_sha256: expectedSha256,
      purpose,
    });
    onProgress?.({ file, state: "uploading" });
    await uploadSessionContent(
      tenantId,
      projectId,
      session.upload_session_id,
      file,
    );
    onProgress?.({ file, state: "completing" });
    const result = await completeUploadSession(
      tenantId,
      projectId,
      session.upload_session_id,
      {},
    );
    if (
      !["succeeded", "queued", "running", "partial"].includes(result.status)
    ) {
      throw new Error(
        result.error?.message ??
          result.error?.reason ??
          "资料未被受理，请查看服务端返回的处理原因。",
      );
    }
    onProgress?.({ file, state: "accepted", result });
    return result;
  } catch (error) {
    const normalized =
      error instanceof Error ? error : new Error("文件上传失败。");
    onProgress?.({ file, state: "failed", error: normalized });
    throw normalized;
  }
}

export function importKnowledge(
  tenantId: string,
  projectId: string,
  input: ImportBatchInput,
  idempotencyKey = createIdempotencyKey(),
): Promise<ImportBatchResult> {
  return apiFetch<unknown>("/knowledge/imports", {
    ...scopedOptions(tenantId, projectId),
    method: "POST",
    body: input,
    idempotencyKey,
  }).then(normalizeImportBatch);
}

/**
 * Materializes the W02 initial-source configuration into scoped W03 sources.
 * The server derives the stable source hash, so retries are safe without a
 * client-generated import identity.
 */
export function materializeInitialSources(
  tenantId: string,
  projectId: string,
): Promise<ImportBatchResult> {
  return apiFetch<unknown>("/knowledge/materialize-initial-sources", {
    ...scopedOptions(tenantId, projectId),
    method: "POST",
  }).then(normalizeImportBatch);
}

export function searchKnowledge(
  tenantId: string,
  projectId: string,
  input: SearchInput,
): Promise<SearchResult> {
  return apiFetch<SearchResult>("/knowledge/search", {
    ...scopedOptions(tenantId, projectId),
    method: "POST",
    body: input,
  });
}

export async function askKnowledge(
  tenantId: string,
  projectId: string,
  input: AskInput,
): Promise<AskResult> {
  return normalizeAskResult(
    await apiFetch<unknown>("/knowledge/ask", {
      ...scopedOptions(tenantId, projectId),
      method: "POST",
      body: input,
    }),
  );
}

function useScope(tenantId: string | undefined, projectId: string | undefined) {
  const { session } = useAuth();
  return scopeFor(session, tenantId, projectId);
}

export function useKnowledgeCapabilitiesQuery(
  tenantId: string | undefined,
  projectId: string | undefined,
) {
  const scope = useScope(tenantId, projectId);
  return useQuery({
    queryKey: scope
      ? knowledgeQueryKeys.capabilities(scope)
      : [
          ...knowledgeQueryKeys.all,
          "capabilities",
          "anonymous",
          "",
          tenantId ?? "",
          projectId ?? null,
        ],
    queryFn: () => getKnowledgeCapabilities(tenantId!, projectId!),
    enabled: Boolean(scope),
  });
}

export function useSourcesQuery(
  tenantId: string | undefined,
  projectId: string | undefined,
  query = "",
) {
  const scope = useScope(tenantId, projectId);
  return useQuery({
    queryKey: scope
      ? knowledgeQueryKeys.sources(scope, query)
      : [
          ...knowledgeQueryKeys.all,
          "sources",
          "anonymous",
          "",
          tenantId ?? "",
          projectId ?? null,
          query,
        ],
    queryFn: () => listSources(tenantId!, projectId!, query),
    enabled: Boolean(scope),
  });
}

export function useSourceQuery(
  tenantId: string | undefined,
  projectId: string | undefined,
  sourceId: string | undefined,
) {
  const scope = useScope(tenantId, projectId);
  return useQuery({
    queryKey:
      scope && sourceId
        ? knowledgeQueryKeys.source(scope, sourceId)
        : [
            ...knowledgeQueryKeys.all,
            "source",
            "anonymous",
            "",
            tenantId ?? "",
            projectId ?? null,
            sourceId ?? "",
          ],
    queryFn: () => getSource(tenantId!, projectId!, sourceId!),
    enabled: Boolean(scope && sourceId),
  });
}

export function useProductsQuery(
  tenantId: string | undefined,
  projectId: string | undefined,
) {
  const scope = useScope(tenantId, projectId);
  return useQuery({
    queryKey: scope
      ? knowledgeQueryKeys.products(scope)
      : [
          ...knowledgeQueryKeys.all,
          "products",
          "anonymous",
          "",
          tenantId ?? "",
          projectId ?? null,
        ],
    queryFn: () => listProducts(tenantId!, projectId!),
    enabled: Boolean(scope),
  });
}

export function useFactsQuery(
  tenantId: string | undefined,
  projectId: string | undefined,
  productId: string | null,
  query = "",
) {
  const scope = useScope(tenantId, projectId);
  return useQuery({
    queryKey: scope
      ? knowledgeQueryKeys.facts(scope, productId, query)
      : [
          ...knowledgeQueryKeys.all,
          "facts",
          "anonymous",
          "",
          tenantId ?? "",
          projectId ?? null,
          productId,
          query,
        ],
    queryFn: () => listFacts(tenantId!, projectId!, productId, query),
    enabled: Boolean(scope),
  });
}

export function useKnowledgeReleaseQuery(
  tenantId: string | undefined,
  projectId: string | undefined,
) {
  const scope = useScope(tenantId, projectId);
  return useQuery({
    queryKey: scope
      ? knowledgeQueryKeys.release(scope)
      : [
          ...knowledgeQueryKeys.all,
          "release",
          "anonymous",
          "",
          tenantId ?? "",
          projectId ?? null,
        ],
    queryFn: () => getCurrentKnowledgeRelease(tenantId!, projectId!),
    enabled: Boolean(scope),
  });
}

function invalidateKnowledge(
  queryClient: ReturnType<typeof useQueryClient>,
  scope: QueryScope | undefined,
) {
  if (!scope) return Promise.resolve();
  return queryClient.invalidateQueries({ queryKey: knowledgeQueryKeys.all });
}

export function useImportKnowledgeMutation(
  tenantId: string | undefined,
  projectId: string | undefined,
) {
  const queryClient = useQueryClient();
  const scope = useScope(tenantId, projectId);
  return useMutation({
    mutationFn: (input: ImportBatchInput) => {
      if (!tenantId || !projectId) throw new Error("请先选择项目。");
      return importKnowledge(tenantId, projectId, input);
    },
    onSuccess: () => invalidateKnowledge(queryClient, scope),
  });
}

export function useUploadFilesMutation(
  tenantId: string | undefined,
  projectId: string | undefined,
) {
  const queryClient = useQueryClient();
  const scope = useScope(tenantId, projectId);
  return useMutation({
    mutationFn: async ({
      files,
      purpose,
      onProgress,
    }: {
      files: File[];
      purpose: KnowledgePurpose;
      onProgress?: (state: FileUploadProgress) => void;
    }) => {
      if (!tenantId || !projectId) throw new Error("请先选择项目。");
      const results = await Promise.allSettled(
        files.map((file) =>
          uploadFile(tenantId, projectId, file, purpose, onProgress),
        ),
      );
      return results;
    },
    onSuccess: () => invalidateKnowledge(queryClient, scope),
  });
}

export function useAskKnowledgeMutation(
  tenantId: string | undefined,
  projectId: string | undefined,
) {
  return useMutation({
    mutationFn: (input: AskInput) => {
      if (!tenantId || !projectId) throw new Error("请先选择项目。");
      return askKnowledge(tenantId, projectId, input);
    },
  });
}
