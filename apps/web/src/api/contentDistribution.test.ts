import { describe, expect, it, vi } from "vitest";
import type { ContentRevision, RichNode } from "./content";
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

const revision = (node?: RichNode): ContentRevision =>
  ({
    revision_id: "rev-1",
    asset_id: "asset-1",
    document: {
      title: "Article",
      schema_version: 2,
      blocks: [
        {
          block_id: "block-1",
          kind: node ? "rich" : "paragraph",
          text: node ? "" : "Plain text",
          citation_ids: [],
          items: [],
          ...(node ? { rich: { version: 1, node } } : {}),
        },
      ],
    },
  }) as unknown as ContentRevision;

describe("single-article distribution API", () => {
  it("keeps current plain documents on the Markdown path", () => {
    const plain = revision();
    plain.document.blocks.push(
      { ...plain.document.blocks[0], block_id: "heading", kind: "heading" },
      {
        ...plain.document.blocks[0],
        block_id: "list",
        kind: "list",
        text: "",
        items: ["One"],
      },
    );
    expect(distributionFormat(plain)).toBe("markdown.v1");
  });

  it.each<RichNode>([
    { type: "media" },
    { type: "table" },
    { type: "codeBlock", content: [{ type: "text", text: "code" }] },
    {
      type: "paragraph",
      content: [{ type: "text", text: "Bold", marks: [{ type: "bold" }] }],
    },
    {
      type: "paragraph",
      content: [
        {
          type: "text",
          text: "Link",
          marks: [{ type: "link", attrs: { href: "https://example.test" } }],
        },
      ],
    },
    { type: "orderedList" },
  ])("requires rich distribution for a rich $type block", (node) => {
    expect(distributionFormat(revision(node))).toBe("rich_markdown.v2");
  });

  it.each(["primary", "short_post"] as const)(
    "posts a frozen %s input with a caller-owned retry key",
    async (placement) => {
      const input = {
        content_asset_id: "asset-1",
        content_revision_id: "rev-1",
        account_id: "account-1",
        placement_slot: placement,
        format: "markdown.v1" as const,
        withdraw_after_publish: false,
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
    },
  );

  it("preserves an explicit withdrawal instruction in the frozen request", async () => {
    const input = {
      content_asset_id: "asset-1",
      content_revision_id: "rev-1",
      account_id: "account-1",
      placement_slot: "short_post" as const,
      format: "markdown.v1" as const,
      withdraw_after_publish: true,
    };
    await submitContentDistributionRequest(
      "tenant-1",
      "project-1",
      input,
      "retry-withdrawal",
    );
    expect(apiFetch).toHaveBeenLastCalledWith(
      "/projects/project-1/content-distribution-requests",
      {
        tenantId: "tenant-1",
        projectId: "project-1",
        method: "POST",
        body: input,
        idempotencyKey: "retry-withdrawal",
      },
    );
  });
});
