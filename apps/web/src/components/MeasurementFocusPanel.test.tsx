import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MeasurementFocusPanel } from "./MeasurementFocusPanel";
import { ApiError } from "../api/client";
import i18n from "../i18n";

const state = vi.hoisted(() => ({
  hasData: true,
  isPending: false,
  isError: false,
  data: {
    revision: 7,
    settings: {
      brand_name: "Example",
      product_name: "Example X",
      competitors: ["Other X"],
      measurement_targets: [
        { name: "Example X", kind: "product", aliases: [] as string[] },
      ],
    },
  },
  mutateAsync: vi.fn(),
  refetch: vi.fn(),
}));

vi.mock("../api/projects", () => ({
  useProjectQuery: () => ({
    data: state.hasData ? state.data : undefined,
    isPending: state.isPending,
    isError: state.isError,
    refetch: state.refetch,
  }),
  useUpdateProjectMutation: () => ({
    mutateAsync: state.mutateAsync,
    isPending: false,
  }),
}));

beforeEach(async () => {
  vi.clearAllMocks();
  state.hasData = true;
  state.isPending = false;
  state.isError = false;
  state.mutateAsync.mockResolvedValue({});
  state.data = {
    revision: 7,
    settings: {
      brand_name: "Example",
      product_name: "Example X",
      competitors: ["Other X"],
      measurement_targets: [
        { name: "Example X", kind: "product", aliases: [] as string[] },
      ],
    },
  };
  await i18n.changeLanguage("zh-CN");
});
afterEach(cleanup);

function renderPanel(canWrite = true) {
  return render(
    <MeasurementFocusPanel
      tenantId="tenant"
      projectId="project"
      canWrite={canWrite}
    />,
  );
}

describe("measurement focus panel", () => {
  it.each(["loading", "failed"])(
    "does not present an unread target as unset in compact mode: %s",
    (status) => {
      state.hasData = false;
      state.isPending = status === "loading";
      state.isError = status === "failed";
      render(
        <MeasurementFocusPanel
          tenantId="tenant"
          projectId="project"
          canWrite
          compact
        />,
      );
      expect(
        screen.getByText(
          status === "loading" ? "正在读取关注对象…" : "暂时无法读取关注对象",
        ),
      ).toBeVisible();
      expect(screen.queryByText("未设置")).not.toBeInTheDocument();
      expect(
        screen.queryByText("也可以不设置，直接测量任何问题。"),
      ).not.toBeInTheDocument();
      expect(
        screen.queryByRole("button", { name: "设置关注对象" }),
      ).not.toBeInTheDocument();
      if (status === "failed")
        expect(screen.getByRole("button", { name: "重试" })).toBeVisible();
    },
  );
  it("shows the saved product and competitors without an initial form", () => {
    renderPanel();
    expect(screen.getByText(/Example X · 产品/)).toBeVisible();
    expect(screen.getByText("对比对象：Other X")).toBeVisible();
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
  });

  it("saves a partial revision-bound edit and leaves unrelated settings alone", async () => {
    renderPanel();
    fireEvent.click(screen.getByRole("button", { name: "修改" }));
    fireEvent.change(screen.getByRole("textbox", { name: "名称" }), {
      target: { value: "Example Y" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() =>
      expect(state.mutateAsync).toHaveBeenCalledWith({
        revision: 7,
        settings: {
          measurement_targets: [
            { name: "Example Y", kind: "product", aliases: [] },
          ],
        },
      }),
    );
    await waitFor(() =>
      expect(screen.queryByRole("textbox")).not.toBeInTheDocument(),
    );
  });

  it("allows no target and does not require setup for measurement", () => {
    state.data.settings.brand_name = "";
    state.data.settings.product_name = "";
    state.data.settings.measurement_targets = [];
    renderPanel();
    expect(screen.getByText("也可以不设置，直接测量任何问题。")).toBeVisible();
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
  });

  it("clears targets with an empty list without changing enterprise metadata", async () => {
    renderPanel();
    fireEvent.click(screen.getByRole("button", { name: "修改" }));
    fireEvent.click(screen.getByRole("button", { name: "移除关注对象 1" }));
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() =>
      expect(state.mutateAsync).toHaveBeenCalledWith({
        revision: 7,
        settings: { measurement_targets: [] },
      }),
    );
  });

  it("preserves the draft on a concurrent update instead of overwriting it", async () => {
    state.mutateAsync.mockRejectedValue(
      new ApiError(409, { code: "conflict" }, "Conflict"),
    );
    renderPanel();
    fireEvent.click(screen.getByRole("button", { name: "修改" }));
    fireEvent.change(screen.getByRole("textbox", { name: "名称" }), {
      target: { value: "Example Y" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    expect(
      await screen.findByText("项目信息已更新，请重新打开修改。"),
    ).toBeVisible();
    expect(screen.getByRole("textbox", { name: "名称" })).toHaveValue(
      "Example Y",
    );
    expect(state.refetch).toHaveBeenCalledOnce();
  });

  it("adds independent targets with explicit aliases and types", async () => {
    renderPanel();
    fireEvent.click(screen.getByRole("button", { name: "修改" }));
    fireEvent.click(screen.getByRole("button", { name: "添加关注对象" }));
    fireEvent.change(screen.getAllByRole("textbox", { name: "名称" })[1], {
      target: { value: "Example table" },
    });
    fireEvent.change(screen.getAllByRole("combobox", { name: "类型" })[1], {
      target: { value: "restaurant" },
    });
    fireEvent.change(
      screen.getAllByRole("textbox", { name: "别名（可选）" })[1],
      { target: { value: "Example dining\nTable short" } },
    );
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() =>
      expect(state.mutateAsync).toHaveBeenCalledWith({
        revision: 7,
        settings: {
          measurement_targets: [
            { name: "Example X", kind: "product", aliases: [] },
            {
              name: "Example table",
              kind: "restaurant",
              aliases: ["Example dining", "Table short"],
            },
          ],
        },
      }),
    );
  });

  it("offers no editing controls to a read-only member", () => {
    renderPanel(false);
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
    expect(screen.getByText(/Example X · 产品/)).toBeVisible();
  });
});
