import { afterEach, describe, expect, it, vi } from "vitest";
import {
  getProjectSerpSettings,
  saveProjectSerpSetting,
  testProjectSerpSetting,
} from "./projectSerp";

afterEach(() => vi.unstubAllGlobals());
describe("project search settings requests", () => {
  it("uses scoped no-store settings routes and sends only saved revision for account testing", async () => {
    const calls: { url: URL; init: RequestInit }[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init: RequestInit = {}) => {
        calls.push({ url: new URL(String(input), "http://localhost"), init });
        return new Response(
          JSON.stringify({ items: [], encryption_available: true }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }),
    );
    await getProjectSerpSettings("tenant", "project");
    await saveProjectSerpSetting("tenant", "project", "primary", {
      expected_revision: 1,
      enabled: false,
      protocol_defaults: {
        query: "",
        engine: "google",
        source: "dataforseo",
        surface: "third_party_api",
        country: "US",
        city: null,
        language: "en",
        device: "desktop",
        requested_depth: 10,
      },
    });
    await testProjectSerpSetting("tenant", "project", "primary", 2);
    expect(calls.map(({ url }) => url.pathname)).toEqual([
      "/api/v1/projects/project/serp-settings",
      "/api/v1/projects/project/serp-settings/primary",
      "/api/v1/projects/project/serp-settings/primary/test",
    ]);
    for (const { url, init } of calls) {
      expect(url.searchParams.get("tenant_id")).toBe("tenant");
      expect(url.searchParams.get("project_id")).toBe("project");
      expect(init.cache).toBe("no-store");
    }
    expect(JSON.parse(String(calls[1].init.body))).not.toHaveProperty("login");
    expect(JSON.parse(String(calls[1].init.body))).not.toHaveProperty(
      "password",
    );
    expect(JSON.parse(String(calls[2].init.body))).toEqual({
      expected_revision: 2,
    });
    expect(
      calls.some(({ url }) => url.pathname.includes("serp-measurements")),
    ).toBe(false);
  });
});
