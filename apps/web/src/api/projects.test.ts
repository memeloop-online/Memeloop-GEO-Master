import { QueryClient } from "@tanstack/react-query";
import { afterEach, describe, expect, it, vi } from "vitest";
import { getProjectStart } from "./projects";

afterEach(() => vi.unstubAllGlobals());

describe("current project start", () => {
  it("stores an absent start as a successful empty query", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response("{}", { status: 404 })),
    );
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    });
    try {
      await expect(
        client.fetchQuery({
          queryKey: ["project-start", "tenant", "project"],
          queryFn: () => getProjectStart("project", "tenant"),
        }),
      ).resolves.toBeNull();
      expect(
        client.getQueryState(["project-start", "tenant", "project"])?.status,
      ).toBe("success");
    } finally {
      client.clear();
    }
  });

  it.each([403, 503])("does not hide an HTTP %s failure", async (status) => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response("{}", { status })),
    );
    await expect(getProjectStart("project", "tenant")).rejects.toMatchObject({
      status,
    });
  });
});
