import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useAuth } from "../auth/AuthProvider";
import { queryScopeFor, type QueryScope } from "../auth/types";
import {
  apiFetch,
  apiRequestUrl,
  createIdempotencyKey,
  type ApiRequestOptions,
} from "./client";

export type AgentMessageRole = "user" | "assistant" | "system" | "tool";
export type AgentConversationStatus = "active" | "archived";
export type AgentTurnStatus =
  "queued" | "running" | "succeeded" | "failed" | "cancelled";
export type AgentRunStatus = AgentTurnStatus;
export type AgentRuntimeCapabilityStatus =
  "available" | "missing" | "unavailable";

/**
 * A durable object reference. Browser File values must be uploaded through a
 * dedicated object-upload flow before they are included in a chat command.
 */
export interface AgentAttachmentReference {
  attachment_id: string;
  object_id: string;
  filename: string;
  media_type?: string;
  size_bytes?: number;
  sha256?: string;
  object_version?: string;
}

export interface AgentConversationSummary {
  id: string;
  title?: string | null;
  status: AgentConversationStatus;
  revision: number;
  created_at: string;
  updated_at: string;
}

export interface AgentRuntimeCapability {
  status: AgentRuntimeCapabilityStatus;
  runtime: string;
  version?: string | null;
  reason?: string | null;
}

export interface AgentMessage {
  id: string;
  conversation_id: string;
  turn_id?: string | null;
  role: AgentMessageRole;
  content: string;
  attachments: AgentAttachmentReference[];
  metadata: unknown;
  sequence: number;
  created_at: string;
}

export interface AgentTurn {
  id: string;
  conversation_id: string;
  root_message_id: string;
  previous_turn_id?: string | null;
  run_id?: string | null;
  status: AgentTurnStatus;
  cancel_version: number;
  created_at: string;
  updated_at: string;
}

export interface AgentRun {
  id: string;
  conversation_id: string;
  turn_id: string;
  status: AgentRunStatus;
  capability: AgentRuntimeCapability;
  error?: unknown;
  cancel_version: number;
  created_at: string;
  updated_at: string;
}

export interface AgentConversationDetail {
  conversation: AgentConversationSummary;
  messages: AgentMessage[];
  turns: AgentTurn[];
  runs: AgentRun[];
}

export interface AgentConversationListResponse {
  items: AgentConversationSummary[];
  next_cursor: string | null;
}

export interface CreateAgentConversationInput {
  title?: string;
}

export interface PostAgentMessageInput {
  content: string;
  /**
   * References are server-owned uploaded objects, never browser File values.
   */
  attachments?: AgentAttachmentReference[];
}

export interface AgentMessageAcceptance {
  status: "accepted";
  conversation_id: string;
  message_id: string;
  turn_id: string;
  run_id: string;
  events_url: string;
  run_status: AgentRunStatus;
  error?: {
    code?: string;
    message?: string;
    details?: unknown;
  } | null;
}

export interface AgentStreamEvent {
  id?: string;
  type?: string;
  data: unknown;
}

function scopeKey(scope: QueryScope) {
  return [
    scope.userId,
    scope.operatorId,
    scope.tenantId,
    scope.projectId ?? null,
  ] as const;
}

export const agentQueryKeys = {
  all: ["agent"] as const,
  conversations: (scope: QueryScope) =>
    [...agentQueryKeys.all, "conversations", ...scopeKey(scope)] as const,
  conversation: (scope: QueryScope, conversationId: string) =>
    [
      ...agentQueryKeys.all,
      "conversation",
      ...scopeKey(scope),
      conversationId,
    ] as const,
};

function scopedOptions(tenantId: string, projectId: string): ApiRequestOptions {
  return { tenantId, projectId };
}

export function listAgentConversations(
  tenantId: string,
  projectId: string,
): Promise<AgentConversationListResponse> {
  return apiFetch<AgentConversationListResponse>("/agent/conversations", {
    ...scopedOptions(tenantId, projectId),
  });
}

export function createAgentConversation(
  tenantId: string,
  projectId: string,
  input: CreateAgentConversationInput = {},
  idempotencyKey = createIdempotencyKey(),
): Promise<AgentConversationSummary> {
  return apiFetch<AgentConversationSummary>("/agent/conversations", {
    ...scopedOptions(tenantId, projectId),
    method: "POST",
    body: input,
    idempotencyKey,
  });
}

export function getAgentConversation(
  tenantId: string,
  projectId: string,
  conversationId: string,
): Promise<AgentConversationDetail> {
  return apiFetch<AgentConversationDetail>(
    `/agent/conversations/${encodeURIComponent(conversationId)}`,
    scopedOptions(tenantId, projectId),
  );
}

export function postAgentMessage(
  tenantId: string,
  projectId: string,
  conversationId: string,
  input: PostAgentMessageInput,
  idempotencyKey = createIdempotencyKey(),
): Promise<AgentMessageAcceptance> {
  return apiFetch<AgentMessageAcceptance>(
    `/agent/conversations/${encodeURIComponent(conversationId)}/messages`,
    {
      ...scopedOptions(tenantId, projectId),
      method: "POST",
      body: input,
      idempotencyKey,
    },
  );
}

export function cancelAgentTurn(
  tenantId: string,
  projectId: string,
  turnId: string,
  idempotencyKey = createIdempotencyKey(),
): Promise<AgentRun> {
  return apiFetch<AgentRun>(
    `/agent/turns/${encodeURIComponent(turnId)}/cancel`,
    {
      ...scopedOptions(tenantId, projectId),
      method: "POST",
      idempotencyKey,
    },
  );
}

/**
 * EventSource sends same-origin cookies but cannot carry the JSON/CSRF request
 * headers used by commands. The scoped selector query and optional durable
 * event cursor are therefore part of its URL.
 */
export function agentEventStreamUrl(
  tenantId: string,
  projectId: string,
  conversationId: string,
  after?: string | number,
) {
  const url = new URL(
    apiRequestUrl(
      `/agent/conversations/${encodeURIComponent(conversationId)}/events`,
      scopedOptions(tenantId, projectId),
    ),
    window.location.origin,
  );
  if (after !== undefined) url.searchParams.set("after", String(after));
  return `${url.pathname}${url.search}`;
}

export function openAgentEventStream(
  url: string,
  onEvent: (event: AgentStreamEvent) => void,
  onError?: () => void,
) {
  const source = new EventSource(url);
  source.onmessage = (event) => {
    let data: unknown = event.data;
    try {
      data = JSON.parse(event.data) as unknown;
    } catch {
      // Some SSE producers intentionally use a plain-text progress event.
    }
    onEvent({ id: event.lastEventId || undefined, data });
  };
  source.onerror = () => onError?.();
  return source;
}

function useScope(tenantId: string | undefined, projectId: string | undefined) {
  const { session } = useAuth();
  return session && tenantId && projectId
    ? queryScopeFor(session, tenantId, projectId)
    : undefined;
}

export function useAgentConversationsQuery(
  tenantId: string | undefined,
  projectId: string | undefined,
) {
  const scope = useScope(tenantId, projectId);
  return useQuery({
    queryKey: scope
      ? agentQueryKeys.conversations(scope)
      : [
          ...agentQueryKeys.all,
          "conversations",
          "anonymous",
          "",
          tenantId ?? "",
          projectId ?? null,
        ],
    queryFn: () => listAgentConversations(tenantId!, projectId!),
    enabled: Boolean(scope),
  });
}

export function useAgentConversationQuery(
  tenantId: string | undefined,
  projectId: string | undefined,
  conversationId: string | undefined,
) {
  const scope = useScope(tenantId, projectId);
  return useQuery({
    queryKey:
      scope && conversationId
        ? agentQueryKeys.conversation(scope, conversationId)
        : [
            ...agentQueryKeys.all,
            "conversation",
            "anonymous",
            "",
            tenantId ?? "",
            projectId ?? null,
            conversationId ?? "",
          ],
    queryFn: () => getAgentConversation(tenantId!, projectId!, conversationId!),
    enabled: Boolean(scope && conversationId),
  });
}

export function useCreateAgentConversationMutation(
  tenantId: string | undefined,
  projectId: string | undefined,
) {
  const queryClient = useQueryClient();
  const scope = useScope(tenantId, projectId);
  return useMutation({
    mutationFn: (input: CreateAgentConversationInput = {}) => {
      if (!tenantId || !projectId) throw new Error("请先选择项目。");
      return createAgentConversation(tenantId, projectId, input);
    },
    onSuccess: async () => {
      if (!scope) return;
      await queryClient.invalidateQueries({
        queryKey: agentQueryKeys.conversations(scope),
      });
    },
  });
}
