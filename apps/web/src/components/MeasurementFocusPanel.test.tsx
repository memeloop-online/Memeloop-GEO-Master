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
  data: {
    revision: 7,
    settings: {
      brand_name: "Example",
      product_name: "Example X",
      competitors: ["Other X"],
    },
  },
  mutateAsync: vi.fn(),
  refetch: vi.fn(),
}));

vi.mock("../api/projects", () => ({
  useProjectQuery: () => ({
    data: state.data,
    isPending: false,
    isError: false,
    refetch: state.refetch,
  }),
  useUpdateProjectMutation: () => ({
    mutateAsync: state.mutateAsync,
    isPending: false,
  }),
}));

beforeEach(async () => {
  vi.clearAllMocks();
  state.mutateAsync.mockResolvedValue({});
  state.data = {
    revision: 7,
    settings: {
      brand_name: "Example",
      product_name: "Example X",
      competitors: ["Other X"],
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
  it("shows the saved product and competitors without an initial form", () => {
    renderPanel();
    expect(screen.getByText("Example X")).toBeVisible();
    expect(screen.getByText("对比对象：Other X")).toBeVisible();
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
  });

  it("saves a partial revision-bound edit and leaves unrelated settings alone", async () => {
    renderPanel();
    fireEvent.click(screen.getByRole("button", { name: "修改" }));
    fireEvent.change(
      screen.getByRole("textbox", { name: "具体产品（可选）" }),
      {
        target: { value: "Example Y" },
      },
    );
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() =>
      expect(state.mutateAsync).toHaveBeenCalledWith({
        revision: 7,
        settings: { brand_name: "Example", product_name: "Example Y" },
      }),
    );
    await waitFor(() =>
      expect(screen.queryByRole("textbox")).not.toBeInTheDocument(),
    );
  });

  it("allows no target and does not require setup for measurement", () => {
    state.data.settings.brand_name = "";
    state.data.settings.product_name = "";
    renderPanel();
    expect(screen.getByText("也可以不设置，直接测量任何问题。")).toBeVisible();
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
  });

  it("clears the specific product with the existing nullable API field", async () => {
    renderPanel();
    fireEvent.click(screen.getByRole("button", { name: "修改" }));
    fireEvent.change(
      screen.getByRole("textbox", { name: "具体产品（可选）" }),
      {
        target: { value: "" },
      },
    );
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() =>
      expect(state.mutateAsync).toHaveBeenCalledWith({
        revision: 7,
        settings: { brand_name: "Example", product_name: null },
      }),
    );
  });

  it("preserves the draft on a concurrent update instead of overwriting it", async () => {
    state.mutateAsync.mockRejectedValue(
      new ApiError(409, { code: "conflict" }, "Conflict"),
    );
    renderPanel();
    fireEvent.click(screen.getByRole("button", { name: "修改" }));
    fireEvent.change(
      screen.getByRole("textbox", { name: "具体产品（可选）" }),
      {
        target: { value: "Example Y" },
      },
    );
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    expect(
      await screen.findByText("项目信息已更新，请重新打开修改。"),
    ).toBeVisible();
    expect(
      screen.getByRole("textbox", { name: "具体产品（可选）" }),
    ).toHaveValue("Example Y");
    expect(state.refetch).toHaveBeenCalledOnce();
  });

  it("offers no editing controls to a read-only member", () => {
    renderPanel(false);
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
    expect(screen.getByText("Example X")).toBeVisible();
  });
});
