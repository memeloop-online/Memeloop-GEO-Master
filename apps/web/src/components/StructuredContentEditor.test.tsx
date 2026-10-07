import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import "../i18n";
import type { StructuredDocument } from "../api/content";
import { StructuredContentEditor } from "./StructuredContentEditor";

const find = vi.hoisted(() => vi.fn());
const bytes = vi.hoisted(() => vi.fn());
const list = vi.hoisted(() => vi.fn());
vi.mock("../api/contentMedia", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../api/contentMedia")>()),
  findContentMediaBinding: find,
  readContentMediaBytes: bytes,
  listContentMedia: list,
}));

const key = {
  object_id: "1e47ee2e-534a-4695-a998-46a32639d0b2",
  object_version: 2,
  sha256: "a".repeat(64),
};
const mediaDocument: StructuredDocument = {
  title: "Guide",
  schema_version: 2,
  blocks: [
    {
      block_id: "media-1",
      kind: "rich",
      text: "",
      citation_ids: [],
      items: [],
      rich: {
        version: 1,
        node: {
          type: "media",
          attrs: { ...key, alt: "A diagram", caption: "Figure 1" },
        },
      },
    },
  ],
};

afterEach(() => {
  vi.clearAllMocks();
});

describe("read-only content media preview", () => {
  it("inserts selected media as a persistable rich block without altering previous evidence", async () => {
    const onChange = vi.fn();
    const previous: StructuredDocument = {
      title: "Guide",
      blocks: [
        {
          block_id: "before",
          kind: "paragraph",
          text: "Before",
          items: [],
          citation_ids: ["evidence-1"],
        },
      ],
    };
    list.mockResolvedValue({
      items: [
        {
          binding_id: "binding-1",
          state: "active",
          image: { key, media_type: "image/png", width: 10, height: 20 },
        },
      ],
      next_cursor: null,
    });
    find.mockResolvedValue({ binding_id: "binding-1", image: { key } });
    bytes.mockResolvedValue(new Blob(["bytes"], { type: "image/png" }));
    Object.defineProperty(URL, "createObjectURL", {
      configurable: true,
      value: vi.fn().mockReturnValue("blob:test"),
    });
    Object.defineProperty(URL, "revokeObjectURL", {
      configurable: true,
      value: vi.fn(),
    });
    render(
      <FluentProvider theme={webLightTheme}>
        <StructuredContentEditor
          document={previous}
          mediaScope={{ tenantId: "tenant-1", projectId: "project-1" }}
          readonly={false}
          onChange={onChange}
        />
      </FluentProvider>,
    );
    fireEvent.click(screen.getByRole("button", { name: "插入图片" }));
    fireEvent.click(
      await screen.findByRole("button", { name: /image\/png · 10 × 20/ }),
    );
    fireEvent.click(screen.getByRole("button", { name: "插入所选图片" }));
    await waitFor(() =>
      expect(onChange).toHaveBeenCalledWith(
        expect.objectContaining({
          blocks: [
            previous.blocks[0],
            expect.objectContaining({
              kind: "rich",
              rich: {
                version: 1,
                node: {
                  type: "media",
                  attrs: { ...key, alt: "图片", caption: "" },
                },
              },
            }),
          ],
        }),
        null,
      ),
    );
  });

  it("fetches authenticated image bytes and revokes the object URL on unmount", async () => {
    const create = vi.fn().mockReturnValue("blob:private-image");
    const revoke = vi.fn();
    Object.defineProperty(URL, "createObjectURL", {
      configurable: true,
      value: create,
    });
    Object.defineProperty(URL, "revokeObjectURL", {
      configurable: true,
      value: revoke,
    });
    find.mockResolvedValue({ binding_id: "binding-1", image: { key } });
    bytes.mockResolvedValue(new Blob(["bytes"], { type: "image/png" }));
    const view = render(
      <FluentProvider theme={webLightTheme}>
        <StructuredContentEditor
          document={mediaDocument}
          readonly
          mediaScope={{ tenantId: "tenant-1", projectId: "project-1" }}
          onChange={vi.fn()}
        />
      </FluentProvider>,
    );
    expect(
      await screen.findByRole("img", { name: "A diagram" }),
    ).toHaveAttribute("src", "blob:private-image");
    expect(screen.getByText("Figure 1")).toBeInTheDocument();
    expect(bytes).toHaveBeenCalledWith(
      "tenant-1",
      "project-1",
      "binding-1",
      expect.any(AbortSignal),
    );
    view.unmount();
    expect(revoke).toHaveBeenCalledWith("blob:private-image");
  });

  it("keeps the image reference visible as unavailable when its binding was withdrawn", async () => {
    find.mockResolvedValue(null);
    const onChange = vi.fn();
    render(
      <FluentProvider theme={webLightTheme}>
        <StructuredContentEditor
          document={mediaDocument}
          readonly
          mediaScope={{ tenantId: "tenant-1", projectId: "project-1" }}
          onChange={onChange}
        />
      </FluentProvider>,
    );
    await waitFor(() =>
      expect(
        screen.getByText("图片目前无法查看，引用仍保留。"),
      ).toBeInTheDocument(),
    );
    expect(onChange).not.toHaveBeenCalled();
  });

  it("does not discard a persisted media reference when no project context is available", async () => {
    const onChange = vi.fn();
    render(
      <FluentProvider theme={webLightTheme}>
        <StructuredContentEditor
          document={mediaDocument}
          readonly
          onChange={onChange}
        />
      </FluentProvider>,
    );
    expect(
      await screen.findByText("图片目前无法查看，引用仍保留。"),
    ).toBeInTheDocument();
    expect(find).not.toHaveBeenCalled();
    expect(onChange).not.toHaveBeenCalled();
  });
});
