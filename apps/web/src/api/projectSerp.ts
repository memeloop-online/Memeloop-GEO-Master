import { apiFetch } from "./client";
import type { SerpProtocol } from "./serp";

export interface ProjectSerpSetting {
  source_key: string;
  provider: "dataforseo";
  revision: number;
  enabled: boolean;
  protocol_defaults: SerpProtocol;
  credentials_present: boolean;
  active_credential_revision: number | null;
}
export interface ProjectSerpSettings {
  items: ProjectSerpSetting[];
  encryption_available: boolean;
}
export interface ProjectSerpInput {
  expected_revision: number;
  enabled: boolean;
  protocol_defaults: SerpProtocol;
  login?: string;
  password?: string;
}
export interface ProjectSerpTestResult {
  source_key: string;
  revision: number;
  status:
    "connected" | "authentication_failed" | "unavailable" | "invalid_response";
  checked_at: string;
}
const path = (projectId: string) =>
  `/projects/${encodeURIComponent(projectId)}/serp-settings`;
export function getProjectSerpSettings(tenantId: string, projectId: string) {
  return apiFetch<ProjectSerpSettings>(path(projectId), {
    tenantId,
    projectId,
    cache: "no-store",
  });
}
export function saveProjectSerpSetting(
  tenantId: string,
  projectId: string,
  sourceKey: string,
  body: ProjectSerpInput,
) {
  return apiFetch<ProjectSerpSetting>(
    `${path(projectId)}/${encodeURIComponent(sourceKey)}`,
    { tenantId, projectId, method: "PUT", body, cache: "no-store" },
  );
}
export function testProjectSerpSetting(
  tenantId: string,
  projectId: string,
  sourceKey: string,
  revision: number,
) {
  return apiFetch<ProjectSerpTestResult>(
    `${path(projectId)}/${encodeURIComponent(sourceKey)}/test`,
    {
      tenantId,
      projectId,
      method: "POST",
      body: { expected_revision: revision },
      cache: "no-store",
    },
  );
}
