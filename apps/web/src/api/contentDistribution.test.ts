import { describe, expect, it, vi } from "vitest";
import type { ContentRevision } from "./content";
import {
  distributionFormat,
  getContentDistributionPublication,
  getContentDistributionRequest,
  submitContentDistributionRequest,
} from "./contentDistribution";

vi.mock("./client", () => ({
  apiFetch: vi.fn(async (_path: string, _options: unknown) => ({
    request_id: "req-1",
  })),
}));

import { apiFetch } from "./client";

const revision = (rich: boolean): ContentRevision =>
  ({
    revision_id: "rev-1",
    asset_id: "asset-1",
    document: {
      title: "Article",
      ...(rich ? { schema_version: 2 } : {}),
      blocks: [
        {
          block_id: "block-1",
          kind: rich ? "rich" : "paragraph",
          text: "",
          citation_ids: [],
          items: [],
          ...(rich ? { rich: { version: 1, node: { type: "media" } } } : {}),
        },
      ],
    },
  }) as unknown as ContentRevision;

describe("single-article distribution API", () => {
  it("keeps rich/media revisions rich and never turns them into text", () => {
    expect(distributionFormat(revision(false))).toBe("markdown.v1");
    expect(distributionFormat(revision(true))).toBe("rich_markdown.v2");
    expect(
      distributionFormat({
        ...revision(false),
        document: {
          title: "Nested image",
          blocks: [
            {
              block_id: "block-1",
              kind: "rich",
              text: "",
              citation_ids: [],
              items: [],
              rich: {
                version: 1,
                node: { type: "doc", content: [{ type: "media" }] },
              },
            },
          ],
        },
      }),
    ).toBe("rich_markdown.v2");
  });

  it("posts only the strict frozen input with a caller-owned retry key", async () => {
    const input = {
      content_asset_id: "asset-1",
      content_revision_id: "rev-1",
      account_id: "account-1",
      placement_slot: "primary",
      format: "markdown.v1" as const,
    };
    await submitContentDistributionRequest(
      "tenant-1",
      "project-1",
      input,
      "retry-1",
    );
    expect(apiFetch).toHaveBeenCalledWith(
      "/projects/project-1/content-distribution-requests",
      {
        tenantId: "tenant-1",
        projectId: "project-1",
        method: "POST",
        body: input,
        idempotencyKey: "retry-1",
      },
    );
    await getContentDistributionRequest("tenant-1", "project-1", "req-1");
    expect(apiFetch).toHaveBeenLastCalledWith(
      "/projects/project-1/content-distribution-requests/req-1",
      { tenantId: "tenant-1", projectId: "project-1" },
    );
    await getContentDistributionPublication("tenant-1", "project-1", "req-1");
    expect(apiFetch).toHaveBeenLastCalledWith(
      "/projects/project-1/content-distribution-requests/req-1/publication",
      { tenantId: "tenant-1", projectId: "project-1" },
    );
  });
});
