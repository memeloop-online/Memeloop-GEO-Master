import { afterEach, describe, expect, it, vi } from "vitest";
import {
  createObservationAnalysis,
  listObservationAnalyses,
} from "./observationAnalysis";

afterEach(() => vi.unstubAllGlobals());

describe("saved analysis API", () => {
  it("reads scoped revisions and sources with pagination", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValue(
        new Response(
          JSON.stringify({ items: [], sources: [], next_after: null }),
        ),
      );
    vi.stubGlobal("fetch", fetch);
    await listObservationAnalyses(
      "tenant",
      "project",
      "target",
      "attempt",
      "cursor",
    );
    const url = new URL(fetch.mock.calls[0][0], "http://localhost");
    expect(url.pathname).toBe(
      "/api/v1/projects/project/channel-targets/target/attempts/attempt/analyses",
    );
    expect(url.searchParams.get("tenant_id")).toBe("tenant");
    expect(url.searchParams.get("project_id")).toBe("project");
    expect(url.searchParams.get("after")).toBe("cursor");
  });

  it("only submits a saved-source analysis using the same body and header key", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValue(
        new Response(JSON.stringify({ state: "queued" }), { status: 202 }),
      );
    vi.stubGlobal("fetch", fetch);
    await createObservationAnalysis(
      "tenant",
      "project",
      "target",
      "attempt",
      "stable-key",
    );
    const [url, init] = fetch.mock.calls[0];
    expect(String(url)).toContain("/attempts/attempt/analyses?");
    expect(init.method).toBe("POST");
    expect(JSON.parse(init.body)).toEqual({ idempotency_key: "stable-key" });
    expect(new Headers(init.headers).get("Idempotency-Key")).toBe("stable-key");
    expect(fetch).toHaveBeenCalledTimes(1);
  });
});
