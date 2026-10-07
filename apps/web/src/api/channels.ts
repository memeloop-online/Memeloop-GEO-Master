import { useQuery } from "@tanstack/react-query";
import { useAuth } from "../auth/AuthProvider";
import { queryScopeFor } from "../auth/types";
import { apiFetch } from "./client";

export type ChannelPlatformId =
  "zhihu" | "baidu_creator" | "xiaohongshu" | "kimi";

export interface ChannelPlatform {
  id: ChannelPlatformId;
  label: string;
  purpose: "publishing" | "measurement";
  login_supported: boolean;
}

export type ConnectorAvailability =
  | "unavailable"
  | "disabled"
  | "version_mismatch"
  | "unsupported_content_type"
  | "available";

export interface ProjectConnectorCapability {
  platform_id: string;
  placement_slot: string;
  revision: number;
  enabled: boolean;
  content_types: string[];
  availability: ConnectorAvailability;
}

export interface OperatorConnectorCapability extends ProjectConnectorCapability {
  deployed_version?: string | null;
  verified_content_types: string[];
}

export interface ChannelGroup {
  group_id: string;
  project_id: string;
  name: string;
  created_at: string;
}

export interface ChannelAccount {
  account_id: string;
  project_id: string;
  platform: ChannelPlatformId;
  group_id: string | null;
  status: "needs_login" | "unverified" | "ready" | "disabled" | "expired";
  display_name: string | null;
  platform_account_id: string | null;
  avatar_url: string | null;
  enabled: boolean;
  proxy_configured: boolean;
  proxy_server: string | null;
  created_at: string;
  updated_at: string;
  owner_kind?: "customer" | "operator_pool";
}

export interface PoolGroup {
  group_id: string;
  name: string;
  created_at: string;
}

export interface PoolAccount extends Omit<
  ChannelAccount,
  "project_id" | "owner_kind"
> {
  owner_kind?: "operator_pool";
}

export interface PoolAssignment {
  tenant_id: string;
  project_id: string;
}

export interface ProxyInput {
  server: string;
  username?: string;
  password?: string;
}

export interface LoginStatus {
  phase: string;
  identity?: { display_name?: string; platform_account_id?: string } | null;
}

export interface DesktopAuthorization {
  websocket_path: string;
  protocol: string;
}

const scope = (tenantId: string, projectId: string) => ({
  tenantId,
  projectId,
});
const encoded = (id: string) => encodeURIComponent(id);

export const channelKeys = {
  platforms: ["channel-platforms"] as const,
  accounts: (
    userId: string,
    operatorId: string,
    tenantId: string,
    projectId: string,
  ) => ["channel-accounts", userId, operatorId, tenantId, projectId] as const,
  groups: (
    userId: string,
    operatorId: string,
    tenantId: string,
    projectId: string,
  ) => ["channel-groups", userId, operatorId, tenantId, projectId] as const,
  projectCapabilities: (
    userId: string,
    operatorId: string,
    tenantId: string,
    projectId: string,
  ) =>
    [
      "project-connector-capabilities",
      userId,
      operatorId,
      tenantId,
      projectId,
    ] as const,
  operatorCapabilities: (userId: string, operatorId: string) =>
    ["operator-connector-capabilities", userId, operatorId] as const,
};

export const listProjectConnectorCapabilities = (
  tenantId: string,
  projectId: string,
) =>
  apiFetch<{ items: ProjectConnectorCapability[] }>(
    `/projects/${encoded(projectId)}/connector-capabilities`,
    { tenantId },
  );
export const listOperatorConnectorCapabilities = () =>
  apiFetch<{ items: OperatorConnectorCapability[] }>(
    "/operator/connector-capabilities",
  );
export const updateOperatorConnectorCapability = (
  platformId: string,
  placementSlot: string,
  update: {
    expected_revision: number;
    enabled: boolean;
    content_types: string[];
  },
) =>
  apiFetch<OperatorConnectorCapability>(
    `/operator/connector-capabilities/${encoded(platformId)}/${encoded(placementSlot)}`,
    { method: "PATCH", body: update },
  );

export const listChannelPlatforms = () =>
  apiFetch<{ items: ChannelPlatform[] }>("/channel-platforms");
export const listChannelAccounts = (tenantId: string, projectId: string) =>
  apiFetch<{ items: ChannelAccount[] }>(
    "/channel-accounts",
    scope(tenantId, projectId),
  );
export const listChannelGroups = (tenantId: string, projectId: string) =>
  apiFetch<{ items: ChannelGroup[] }>(
    "/channel-groups",
    scope(tenantId, projectId),
  );
export const createChannelGroup = (
  tenantId: string,
  projectId: string,
  name: string,
) =>
  apiFetch<ChannelGroup>("/channel-groups", {
    ...scope(tenantId, projectId),
    method: "POST",
    body: { project_id: projectId, name },
  });
export const updateChannelGroup = (
  tenantId: string,
  projectId: string,
  id: string,
  name: string,
) =>
  apiFetch<ChannelGroup>(`/channel-groups/${encoded(id)}`, {
    ...scope(tenantId, projectId),
    method: "PATCH",
    body: { name },
  });
export const deleteChannelGroup = (
  tenantId: string,
  projectId: string,
  id: string,
) =>
  apiFetch<void>(`/channel-groups/${encoded(id)}`, {
    ...scope(tenantId, projectId),
    method: "DELETE",
  });
export const createChannelAccount = (
  tenantId: string,
  projectId: string,
  platform: ChannelPlatformId,
  groupId?: string,
  proxy?: ProxyInput,
) =>
  apiFetch<ChannelAccount>("/channel-accounts", {
    ...scope(tenantId, projectId),
    method: "POST",
    body: {
      project_id: projectId,
      platform,
      group_id: groupId ?? null,
      ...(proxy ? { proxy } : {}),
    },
  });
export const updateChannelAccount = (
  tenantId: string,
  projectId: string,
  id: string,
  update: {
    group_id?: string | null;
    enabled?: boolean;
    proxy?: ProxyInput | null;
  },
) =>
  apiFetch<ChannelAccount>(`/channel-accounts/${encoded(id)}`, {
    ...scope(tenantId, projectId),
    method: "PATCH",
    body: update,
  });
export const startChannelLogin = (
  tenantId: string,
  projectId: string,
  accountId: string,
) =>
  apiFetch<{ session_id: string; account_id: string; phase: string }>(
    "/channel-login-sessions",
    {
      ...scope(tenantId, projectId),
      method: "POST",
      body: { project_id: projectId, account_id: accountId },
    },
  );
export const getChannelLoginStatus = (
  tenantId: string,
  projectId: string,
  sessionId: string,
) =>
  apiFetch<LoginStatus>(
    `/channel-login-sessions/${encoded(sessionId)}/status`,
    scope(tenantId, projectId),
  );
export const authorizeChannelDesktop = (
  tenantId: string,
  projectId: string,
  sessionId: string,
) =>
  apiFetch<DesktopAuthorization>(
    `/channel-login-sessions/${encoded(sessionId)}/desktop-authorization`,
    {
      ...scope(tenantId, projectId),
      method: "POST",
    },
  );
export const completeChannelLogin = (
  tenantId: string,
  projectId: string,
  sessionId: string,
) =>
  apiFetch<{ account: ChannelAccount }>(
    `/channel-login-sessions/${encoded(sessionId)}/complete`,
    {
      ...scope(tenantId, projectId),
      method: "POST",
    },
  );
export const cancelChannelLogin = (
  tenantId: string,
  projectId: string,
  sessionId: string,
) =>
  apiFetch<void>(`/channel-login-sessions/${encoded(sessionId)}`, {
    ...scope(tenantId, projectId),
    method: "DELETE",
  });

export const listPoolGroups = () =>
  apiFetch<{ items: PoolGroup[] }>("/operator/channel-groups");
export const createPoolGroup = (name: string) =>
  apiFetch<PoolGroup>("/operator/channel-groups", {
    method: "POST",
    body: { name },
  });
export const updatePoolGroup = (id: string, name: string) =>
  apiFetch<PoolGroup>(`/operator/channel-groups/${encoded(id)}`, {
    method: "PATCH",
    body: { name },
  });
export const listPoolAccounts = () =>
  apiFetch<{ items: PoolAccount[] }>("/operator/channel-accounts");
export const createPoolAccount = (
  platform: ChannelPlatformId,
  groupId?: string,
  proxy?: ProxyInput,
) =>
  apiFetch<PoolAccount>("/operator/channel-accounts", {
    method: "POST",
    body: {
      platform,
      group_id: groupId ?? null,
      ...(proxy ? { proxy } : {}),
    },
  });
export const updatePoolAccount = (
  id: string,
  update: {
    group_id?: string | null;
    enabled?: boolean;
    proxy?: ProxyInput | null;
  },
) =>
  apiFetch<PoolAccount>(`/operator/channel-accounts/${encoded(id)}`, {
    method: "PATCH",
    body: update,
  });
export const listPoolAssignments = (accountId: string) =>
  apiFetch<{ items: PoolAssignment[] }>(
    `/operator/channel-accounts/${encoded(accountId)}/assignments`,
  );
export const assignPoolAccount = (accountId: string, target: PoolAssignment) =>
  apiFetch<void>(
    `/operator/channel-accounts/${encoded(accountId)}/assignments`,
    { method: "POST", body: target },
  );
export const unassignPoolAccount = (
  accountId: string,
  target: PoolAssignment,
) =>
  apiFetch<void>(
    `/operator/channel-accounts/${encoded(accountId)}/assignments`,
    { method: "DELETE", body: target },
  );
export const startPoolLogin = (accountId: string) =>
  apiFetch<{ session_id: string; account_id: string; phase: string }>(
    "/operator/channel-login-sessions",
    { method: "POST", body: { account_id: accountId } },
  );
export const getPoolLoginStatus = (sessionId: string) =>
  apiFetch<LoginStatus>(
    `/operator/channel-login-sessions/${encoded(sessionId)}/status`,
  );
export const authorizePoolDesktop = (sessionId: string) =>
  apiFetch<DesktopAuthorization>(
    `/operator/channel-login-sessions/${encoded(sessionId)}/desktop-authorization`,
    { method: "POST" },
  );
export const completePoolLogin = (sessionId: string) =>
  apiFetch<{ account: PoolAccount }>(
    `/operator/channel-login-sessions/${encoded(sessionId)}/complete`,
    { method: "POST" },
  );
export const cancelPoolLogin = (sessionId: string) =>
  apiFetch<void>(`/operator/channel-login-sessions/${encoded(sessionId)}`, {
    method: "DELETE",
  });

export function useChannelData(tenantId?: string, projectId?: string) {
  const { session } = useAuth();
  const active = Boolean(session && tenantId && projectId);
  const queryScope = active
    ? queryScopeFor(session!, tenantId!, projectId!)
    : undefined;
  const keyArgs = queryScope
    ? ([
        queryScope.userId,
        queryScope.operatorId,
        queryScope.tenantId,
        queryScope.projectId!,
      ] as const)
    : (["anonymous", "", tenantId ?? "", projectId ?? ""] as const);
  const accounts = useQuery({
    queryKey: channelKeys.accounts(...keyArgs),
    queryFn: () => listChannelAccounts(tenantId!, projectId!),
    enabled: active,
  });
  const groups = useQuery({
    queryKey: channelKeys.groups(...keyArgs),
    queryFn: () => listChannelGroups(tenantId!, projectId!),
    enabled: active,
  });
  const platforms = useQuery({
    queryKey: channelKeys.platforms,
    queryFn: listChannelPlatforms,
    enabled: active,
  });
  return { accounts, groups, platforms };
}
