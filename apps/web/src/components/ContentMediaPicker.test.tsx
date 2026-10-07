import { afterEach, describe, expect, it, vi } from "vitest";
import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import "../i18n";
import { ContentMediaPicker } from "./ContentMediaPicker";

const upload = vi.hoisted(() => vi.fn());
const bind = vi.hoisted(() => vi.fn());
const list = vi.hoisted(() => vi.fn());
const read = vi.hoisted(() => vi.fn());
vi.mock("../api/agent", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../api/agent")>()),
  uploadAgentAttachment: upload,
}));
vi.mock("../api/contentMedia", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../api/contentMedia")>()),
  bindContentMedia: bind,
  listContentMedia: list,
  readContentMediaBytes: read,
}));

const key = {
  object_id: "1e47ee2e-534a-4695-a998-46a32639d0b2",
  object_version: 2,
  sha256: "a".repeat(64),
};
const binding = {
  binding_id: "binding-1",
  state: "active",
  image: {
    key,
    media_type: "image/png",
    byte_len: 100,
    width: 20,
    height: 10,
  },
};

afterEach(() => {
  vi.clearAllMocks();
  vi.unstubAllGlobals();
});

describe("content media picker", () => {
  it("shows different authenticated thumbnails for same-size project images and revokes their Blob URLs", async () => {
    const create = vi
      .fn()
      .mockReturnValueOnce("blob:first-image")
      .mockReturnValueOnce("blob:second-image");
    const revoke = vi.fn();
    vi.stubGlobal(
      "URL",
      Object.assign(class extends URL {}, {
        createObjectURL: create,
        revokeObjectURL: revoke,
      }),
    );
    const second = { ...binding, binding_id: "binding-2" };
    list.mockResolvedValue({ items: [binding, second], next_cursor: null });
    read.mockImplementation(
      async () => new Blob(["image"], { type: "image/png" }),
    );
    const view = render(
      <FluentProvider theme={webLightTheme}>
        <ContentMediaPicker
          tenantId="tenant-1"
          projectId="project-1"
          onSelect={vi.fn()}
        />
      </FluentProvider>,
    );
    fireEvent.click(screen.getByRole("button", { name: "插入图片" }));
    await waitFor(() =>
      expect(
        view.container.querySelectorAll(".content-media-thumbnail img"),
      ).toHaveLength(2),
    );
    const images = [
      ...view.container.querySelectorAll<HTMLImageElement>(
        ".content-media-thumbnail img",
      ),
    ];
    expect(images.map((image) => image.src)).toEqual([
      "blob:first-image",
      "blob:second-image",
    ]);
    expect(read).toHaveBeenCalledWith(
      "tenant-1",
      "project-1",
      "binding-1",
      expect.any(AbortSignal),
    );
    expect(read).toHaveBeenCalledWith(
      "tenant-1",
      "project-1",
      "binding-2",
      expect.any(AbortSignal),
    );
    view.unmount();
    expect(revoke).toHaveBeenCalledWith("blob:first-image");
    expect(revoke).toHaveBeenCalledWith("blob:second-image");
  });

  it("locks file, metadata and existing selection until the uploaded object is bound", async () => {
    let finishUpload!: (value: unknown) => void;
    upload.mockReturnValueOnce(
      new Promise((resolve) => {
        finishUpload = resolve;
      }),
    );
    bind.mockResolvedValue(binding);
    list.mockResolvedValue({ items: [binding], next_cursor: null });
    read.mockRejectedValue(new Error("preview unavailable"));
    const onSelect = vi.fn();
    render(
      <FluentProvider theme={webLightTheme}>
        <ContentMediaPicker
          tenantId="tenant-1"
          projectId="project-1"
          onSelect={onSelect}
        />
      </FluentProvider>,
    );
    fireEvent.click(screen.getByRole("button", { name: "插入图片" }));
    const fileInput = await screen.findByLabelText("上传图片");
    fireEvent.change(fileInput, {
      target: {
        files: [new File(["image"], "first.png", { type: "image/png" })],
      },
    });
    fireEvent.change(
      screen.getByRole("textbox", { name: "图片说明（无障碍）" }),
      {
        target: { value: "Original alt" },
      },
    );
    fireEvent.change(
      screen.getByRole("textbox", { name: "图片标题（可选）" }),
      {
        target: { value: "Original caption" },
      },
    );
    fireEvent.click(screen.getByRole("button", { name: "上传并插入" }));
    expect(fileInput).toBeDisabled();
    expect(
      screen.getByRole("textbox", { name: "图片说明（无障碍）" }),
    ).toBeDisabled();
    expect(
      screen.getByRole("textbox", { name: "图片标题（可选）" }),
    ).toBeDisabled();
    expect(screen.getByRole("button", { name: "插入图片" })).toBeDisabled();
    expect(
      screen.getByRole("button", { name: /image\/png · 20 × 10/ }),
    ).toBeDisabled();
    fireEvent.change(fileInput, {
      target: {
        files: [new File(["different"], "second.png", { type: "image/png" })],
      },
    });
    await act(async () =>
      finishUpload({
        attachment_id: "attachment-1",
        filename: "first.png",
        ...key,
        object_version: "2",
      }),
    );
    await waitFor(() =>
      expect(onSelect).toHaveBeenCalledWith({
        ...key,
        alt: "Original alt",
        caption: "Original caption",
      }),
    );
    expect(upload).toHaveBeenCalledTimes(1);
  });

  it("uploads, binds, then inserts the immutable reference with editable alt and caption", async () => {
    const onSelect = vi.fn();
    list.mockResolvedValue({ items: [], next_cursor: null });
    upload.mockResolvedValue({
      attachment_id: "attachment-1",
      filename: "diagram.png",
      ...key,
      object_version: "2",
    });
    bind.mockResolvedValue(binding);
    render(
      <FluentProvider theme={webLightTheme}>
        <ContentMediaPicker
          tenantId="tenant-1"
          projectId="project-1"
          onSelect={onSelect}
        />
      </FluentProvider>,
    );
    fireEvent.click(screen.getByRole("button", { name: "插入图片" }));
    fireEvent.change(await screen.findByLabelText("上传图片"), {
      target: {
        files: [new File(["image"], "diagram.png", { type: "image/png" })],
      },
    });
    fireEvent.change(
      screen.getByRole("textbox", { name: "图片说明（无障碍）" }),
      { target: { value: "A diagram" } },
    );
    fireEvent.change(
      screen.getByRole("textbox", { name: "图片标题（可选）" }),
      { target: { value: "Chart one" } },
    );
    expect(onSelect).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "上传并插入" }));
    await waitFor(() =>
      expect(onSelect).toHaveBeenCalledWith({
        ...key,
        alt: "A diagram",
        caption: "Chart one",
      }),
    );
    expect(bind).toHaveBeenCalledWith("tenant-1", "project-1", key);
    expect(upload).toHaveBeenCalledTimes(1);
  });

  it("keeps the uploaded attachment and draft when binding fails, retrying without reupload", async () => {
    const onSelect = vi.fn();
    list.mockResolvedValue({ items: [], next_cursor: null });
    upload.mockResolvedValue({
      attachment_id: "attachment-1",
      filename: "diagram.png",
      ...key,
      object_version: "2",
    });
    bind
      .mockRejectedValueOnce(new Error("unavailable"))
      .mockResolvedValueOnce(binding);
    render(
      <FluentProvider theme={webLightTheme}>
        <ContentMediaPicker
          tenantId="tenant-1"
          projectId="project-1"
          onSelect={onSelect}
        />
      </FluentProvider>,
    );
    fireEvent.click(screen.getByRole("button", { name: "插入图片" }));
    fireEvent.change(await screen.findByLabelText("上传图片"), {
      target: {
        files: [new File(["image"], "diagram.png", { type: "image/png" })],
      },
    });
    fireEvent.click(screen.getByRole("button", { name: "上传并插入" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("附件已保留");
    expect(onSelect).not.toHaveBeenCalled();
    expect(screen.getByText(/附件 diagram.png 已上传/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "重新核验并插入" }));
    await waitFor(() => expect(onSelect).toHaveBeenCalledTimes(1));
    expect(upload).toHaveBeenCalledTimes(1);
  });
});
