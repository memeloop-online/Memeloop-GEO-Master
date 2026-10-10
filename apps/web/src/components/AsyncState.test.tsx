import { act, fireEvent, render, screen } from "@testing-library/react";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import type { ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import i18n from "../i18n";
import { ErrorState, LoadingState, UnauthorizedState } from "./AsyncState";

function renderState(content: ReactNode) {
  return render(
    <FluentProvider theme={webLightTheme}>{content}</FluentProvider>,
  );
}

afterEach(async () => {
  await act(() => i18n.changeLanguage("zh-CN"));
});

describe("shared async states", () => {
  it("localizes English defaults and retry without changing retry behavior", async () => {
    await act(() => i18n.changeLanguage("en"));
    const onRetry = vi.fn();
    renderState(
      <>
        <LoadingState />
        <ErrorState onRetry={onRetry} />
        <UnauthorizedState tenantName="Example workspace" />
      </>,
    );
    expect(screen.getByText("Loading")).toBeInTheDocument();
    expect(screen.getByText("Unable to load right now")).toBeInTheDocument();
    expect(
      screen.getByText("Check your connection and try again."),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(onRetry).toHaveBeenCalledOnce();
    expect(
      screen.getByRole("heading", { name: "Access denied" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/Can't access “Example workspace”/),
    ).toHaveTextContent(
      "You're signed in but don't have access to this workspace.",
    );
  });

  it("preserves Chinese defaults and explicit caller copy", () => {
    renderState(
      <>
        <LoadingState />
        <LoadingState compact label="自定义加载" />
        <ErrorState onRetry={vi.fn()} title="自定义错误" detail="自定义详情" />
        <UnauthorizedState tenantName="示例工作区" detail="自定义说明" />
      </>,
    );
    expect(screen.getByText("正在加载")).toBeInTheDocument();
    expect(screen.getByLabelText("自定义加载")).toBeInTheDocument();
    expect(screen.getByText("自定义错误")).toBeInTheDocument();
    expect(screen.getByText("自定义详情")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "重试" })).toBeInTheDocument();
    expect(
      screen.getByRole("heading", { name: "权限不足" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText("无法访问“示例工作区”。自定义说明"),
    ).toBeInTheDocument();
  });
});
