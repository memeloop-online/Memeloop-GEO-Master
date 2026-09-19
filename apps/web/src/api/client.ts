const apiBaseUrl = (import.meta.env.VITE_API_BASE_URL ?? "/api/v1").replace(
  /\/+$/,
  "",
);

export interface ApiErrorBody {
  code?: string;
  message?: string;
  details?: unknown;
  request_id?: string;
}

export class ApiError extends Error {
  readonly status: number;
  readonly code?: string;
  readonly details?: unknown;
  readonly requestId?: string;

  constructor(status: number, body: ApiErrorBody, fallbackMessage: string) {
    super(body.message ?? fallbackMessage);
    this.name = "ApiError";
    this.status = status;
    this.code = body.code;
    this.details = body.details;
    this.requestId = body.request_id;
  }

  get isRetryable(): boolean {
    return (
      this.status === 0 ||
      this.status === 408 ||
      this.status === 429 ||
      this.status >= 500
    );
  }
}

type UnauthorizedHandler = () => void | Promise<void>;

let csrfToken: string | undefined;
let unauthorizedHandler: UnauthorizedHandler | undefined;

/**
 * Session material only lives in memory. Cookies are HttpOnly and sent by the
 * browser; this value is the synchronizer token for authenticated writes.
 */
export function setCsrfToken(nextToken: string | undefined) {
  csrfToken = nextToken;
}

export function setUnauthorizedHandler(
  handler: UnauthorizedHandler | undefined,
) {
  unauthorizedHandler = handler;
}

export interface ApiRequestOptions extends Omit<RequestInit, "body"> {
  body?: unknown;
  /**
   * Resource selectors only. Authentication and operator/tenant authorization
   * are derived from the session on the server; callers must not add identity
   * headers.
   */
  tenantId?: string;
  projectId?: string;
  idempotencyKey?: string;
  /**
   * File bytes are deliberately not put through the JSON/idempotency
   * middleware used for regular API commands.
   */
  idempotency?: "auto" | "omit";
  /** Send an already encoded request payload (for example a File). */
  rawBody?: BodyInit;
  csrf?: "required" | "omit";
  unauthorized?: "handle" | "ignore";
}

export function createIdempotencyKey(): string {
  if (typeof globalThis.crypto?.randomUUID === "function") {
    return globalThis.crypto.randomUUID();
  }

  return "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(
    /[xy]/g,
    (character) => {
      const random = Math.floor(Math.random() * 16);
      const value = character === "x" ? random : (random & 0x3) | 0x8;
      return value.toString(16);
    },
  );
}

function withResourceSelectors(
  path: string,
  tenantId: string | undefined,
  projectId: string | undefined,
) {
  if (!tenantId && !projectId) return path;
  const url = new URL(path, window.location.origin);
  if (tenantId) url.searchParams.set("tenant_id", tenantId);
  if (projectId) url.searchParams.set("project_id", projectId);
  return `${url.pathname}${url.search}`;
}

/**
 * Builds an API URL using the same resource selectors as apiFetch. This is
 * intentionally exported for browser-native transports such as EventSource,
 * which cannot use apiFetch while still needing the current tenant/project
 * scope.
 */
export function apiRequestUrl(
  path: string,
  {
    tenantId,
    projectId,
  }: Pick<ApiRequestOptions, "tenantId" | "projectId"> = {},
) {
  return `${apiBaseUrl}${withResourceSelectors(path, tenantId, projectId)}`;
}

export async function apiFetch<T>(
  path: string,
  {
    body,
    tenantId,
    projectId,
    idempotencyKey,
    idempotency = "auto",
    rawBody,
    csrf = "required",
    unauthorized = "handle",
    ...init
  }: ApiRequestOptions = {},
): Promise<T> {
  const headers = new Headers(init.headers);
  headers.set("Accept", "application/json");

  const method = (init.method ?? "GET").toUpperCase();
  const isCommand = !["GET", "HEAD", "OPTIONS"].includes(method);
  if (isCommand && idempotency !== "omit" && idempotencyKey) {
    headers.set("Idempotency-Key", idempotencyKey);
  } else if (
    isCommand &&
    idempotency !== "omit" &&
    !path.startsWith("/auth/")
  ) {
    headers.set("Idempotency-Key", createIdempotencyKey());
  }
  if (isCommand && csrf === "required" && csrfToken) {
    headers.set("X-CSRF-Token", csrfToken);
  }
  if (body !== undefined && rawBody === undefined) {
    headers.set("Content-Type", "application/json");
  }

  let response: Response;
  try {
    response = await fetch(apiRequestUrl(path, { tenantId, projectId }), {
      ...init,
      method,
      headers,
      credentials: "same-origin",
      body:
        rawBody !== undefined
          ? rawBody
          : body === undefined
            ? undefined
            : JSON.stringify(body),
    });
  } catch (error) {
    throw new ApiError(
      0,
      {},
      error instanceof Error ? error.message : "无法连接到 API 服务",
    );
  }

  if (!response.ok) {
    const errorBody = await readJson<ApiErrorBody>(response);
    const apiError = new ApiError(
      response.status,
      errorBody,
      `请求失败（HTTP ${response.status}）`,
    );
    if (apiError.status === 401 && unauthorized === "handle") {
      void unauthorizedHandler?.();
    }
    throw apiError;
  }

  if (response.status === 204) {
    return undefined as T;
  }
  return readJson<T>(response);
}

async function readJson<T>(response: Response): Promise<T> {
  const text = await response.text();
  if (!text) return undefined as T;
  try {
    return JSON.parse(text) as T;
  } catch {
    throw new ApiError(response.status, {}, "API 返回了无效的 JSON 响应");
  }
}
