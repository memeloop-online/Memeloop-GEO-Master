import { useQuery } from "@tanstack/react-query";
import { useAuth } from "../auth/AuthProvider";
import { apiFetch } from "./client";

export type AiUsage = "workbench_content" | "observation_analysis";
export interface ProjectAiSetting {
  usage: AiUsage;
  revision: number;
  mode: "inherit" | "custom";
  model: string | null;
  base_url: string | null;
  key_present: boolean;
  prefer_connected_account?: boolean;
  effective: { source: string; configured: boolean; model: string | null };
}
export interface ProjectAiInput {
  expected_revision: number;
  mode: "inherit" | "custom";
  model?: string;
  base_url?: string;
  api_key?: string;
  prefer_connected_account?: boolean;
}
const path = (projectId: string) =>
  `/projects/${encodeURIComponent(projectId)}/ai-settings`;

export const projectAiKey = (
  userId: string,
  operatorId: string,
  tenantId: string,
  projectId: string,
) => ["project-ai-settings", userId, operatorId, tenantId, projectId] as const;

export function useProjectAiSettings(tenantId: string, projectId: string) {
  const { session } = useAuth();
  return useQuery({
    queryKey: projectAiKey(
      session?.user.id ?? "",
      session?.operator.id ?? "",
      tenantId,
      projectId,
    ),
    queryFn: () =>
      apiFetch<{ items: ProjectAiSetting[] }>(path(projectId), {
        tenantId,
        projectId,
        cache: "no-store",
      }),
    enabled: Boolean(session && tenantId && projectId),
    retry: false,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
    gcTime: 0,
  });
}

export function saveProjectAiSetting(
  tenantId: string,
  projectId: string,
  usage: AiUsage,
  body: ProjectAiInput,
) {
  return apiFetch<ProjectAiSetting>(`${path(projectId)}/${usage}`, {
    method: "PUT",
    tenantId,
    projectId,
    body,
  });
}

export function testProjectAiSetting(
  tenantId: string,
  projectId: string,
  usage: AiUsage,
  revision: number,
) {
  return apiFetch<{ success: boolean }>(`${path(projectId)}/${usage}/test`, {
    method: "POST",
    tenantId,
    projectId,
    body: { expected_revision: revision },
  });
}

export function discoverProjectAiModels(
  tenantId: string,
  projectId: string,
  usage: AiUsage,
  revision: number,
) {
  return apiFetch<{ items: { id: string }[] }>(
    `${path(projectId)}/${usage}/models`,
    {
      method: "POST",
      tenantId,
      projectId,
      body: { expected_revision: revision },
    },
  );
}
