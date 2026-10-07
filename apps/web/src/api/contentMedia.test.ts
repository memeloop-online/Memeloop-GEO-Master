import { afterEach, describe, expect, it, vi } from "vitest";
import {
  bindContentMedia,
  listContentMedia,
  mediaKeyFromAttachment,
  readContentMediaBytes,
} from "./contentMedia";
import { setCsrfToken } from "./client";

afterEach(() => {
  setCsrfToken(undefined);
  vi.unstubAllGlobals();
});

const key = {
  object_id: "1e47ee2e-534a-4695-a998-46a32639d0b2",
  object_version: 2,
  sha256: "a".repeat(64),
};

describe("project media transport", () => {
  it("sends only the immutable object identity, scoped to the project", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(
        new Response(JSON.stringify({ binding_id: "binding" })),
      );
    vi.stubGlobal("fetch", fetchMock);
    setCsrfToken("csrf");
    await bindContentMedia("tenant-a", "project-a", key);
    expect(String(fetchMock.mock.calls[0][0])).toContain(
      "/projects/project-a/content-media/bindings?tenant_id=tenant-a&project_id=project-a",
    );
    expect(fetchMock.mock.calls[0][1].headers.get("X-CSRF-Token")).toBe("csrf");
    expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual(key);
  });

  it("reads authenticated image bytes as a Blob, not JSON or a public URL", async () => {
    const bytes = new Uint8Array([137, 80, 78, 71]);
    const fetchMock = vi
      .fn()
      .mockResolvedValue(
        new Response(bytes, { headers: { "Content-Type": "image/png" } }),
      );
    vi.stubGlobal("fetch", fetchMock);
    const blob = await readContentMediaBytes(
      "tenant-a",
      "project-a",
      "binding-a",
    );
    expect(blob.type).toBe("image/png");
    expect(fetchMock.mock.calls[0][1].credentials).toBe("same-origin");
    expect(String(fetchMock.mock.calls[0][0])).toContain(
      "/content-media/bindings/binding-a/bytes?",
    );
  });

  it("rejects attachments lacking a verified numeric object version", () => {
    expect(() =>
      mediaKeyFromAttachment({
        attachment_id: "attachment-a",
        filename: "image.png",
        object_id: key.object_id,
        sha256: key.sha256,
      }),
    ).toThrow();
    expect(
      mediaKeyFromAttachment({
        attachment_id: "attachment-a",
        filename: "image.png",
        object_id: key.object_id,
        object_version: "2",
        sha256: key.sha256,
      }),
    ).toEqual(key);
  });

  it("lists only requested project bindings with a bounded cursor", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(
        new Response(JSON.stringify({ items: [], next_cursor: null })),
      );
    vi.stubGlobal("fetch", fetchMock);
    await listContentMedia("tenant-a", "project-a", "cursor-a");
    expect(String(fetchMock.mock.calls[0][0])).toContain(
      "limit=50&after=cursor-a",
    );
  });
});
