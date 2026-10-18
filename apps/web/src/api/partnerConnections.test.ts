import { afterEach, describe, expect, it, vi } from "vitest";
import * as client from "./client";
import * as api from "./partnerConnections";

afterEach(() => vi.restoreAllMocks());
describe("partner connection API", () => {
  it("sets only the scoped content export permission without caching the command", async () => {
    const fetch = vi.spyOn(client, "apiFetch").mockResolvedValue({});
    await api.setPartnerContentExport(
      "tenant",
      "project",
      "synthetic/id",
      true,
    );
    expect(fetch).toHaveBeenCalledWith(
      "/projects/project/partner-connections/synthetic%2Fid/permissions",
      {
        tenantId: "tenant",
        projectId: "project",
        method: "POST",
        body: { content_export: true },
        cache: "no-store",
        idempotency: "omit",
      },
    );
  });
  it("uses scoped paginated metadata reads without browser caching", async () => {
    const fetch = vi
      .spyOn(client, "apiFetch")
      .mockResolvedValue({ items: [], next_cursor: null });
    await api.listPartnerConnections("tenant", "project", "synthetic cursor");
    await api.listPartnerKeys(
      "tenant",
      "project",
      "connection",
      "synthetic/key",
    );
    expect(fetch).toHaveBeenNthCalledWith(
      1,
      "/projects/project/partner-connections?limit=100&after=synthetic+cursor",
      { tenantId: "tenant", projectId: "project", cache: "no-store" },
    );
    expect(fetch).toHaveBeenNthCalledWith(
      2,
      "/projects/project/partner-connections/connection/keys?limit=100&after=synthetic%2Fkey",
      { tenantId: "tenant", projectId: "project", cache: "no-store" },
    );
  });
  it("issues and rotates once without idempotency storage or implicit expiry", async () => {
    const fetch = vi.spyOn(client, "apiFetch").mockResolvedValue({});
    await api.issuePartnerKey("tenant", "project", "connection");
    await api.rotatePartnerKey("tenant", "project", "connection", "key");
    const options = {
      tenantId: "tenant",
      projectId: "project",
      method: "POST",
      body: { expires_at: null },
      cache: "no-store",
      idempotency: "omit",
    };
    expect(fetch).toHaveBeenNthCalledWith(
      1,
      "/projects/project/partner-connections/connection/keys",
      options,
    );
    expect(fetch).toHaveBeenNthCalledWith(
      2,
      "/projects/project/partner-connections/connection/keys/key/rotate",
      options,
    );
  });
  it("does not retry a failed one-time-secret request", async () => {
    const fetch = vi
      .spyOn(client, "apiFetch")
      .mockRejectedValue(new Error("synthetic-error"));
    await expect(
      api.issuePartnerKey("tenant", "project", "connection"),
    ).rejects.toThrow("synthetic-error");
    expect(fetch).toHaveBeenCalledTimes(1);
  });
});
