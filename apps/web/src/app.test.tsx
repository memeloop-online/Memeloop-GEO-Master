import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { MemoryRouter } from "react-router-dom";
import { AppRoutes } from "./app";

function renderApp(path: string) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <FluentProvider theme={webLightTheme}>
      <QueryClientProvider client={client}>
        <MemoryRouter initialEntries={[path]}>
          <AppRoutes />
        </MemoryRouter>
      </QueryClientProvider>
    </FluentProvider>,
  );
}

describe("workbench routes", () => {
  it("shows the P02 overview with real static demo data", async () => {
    renderApp("/app/acme/northstar/overview");
    expect(
      await screen.findByRole("heading", { name: "Northstar AI 助手" }),
    ).toBeInTheDocument();
    expect(screen.getByText("450 / 500")).toBeInTheDocument();
    expect(screen.getByText("结果待确认")).toBeInTheDocument();
    expect(screen.getByLabelText("自动优化闭环进度")).toBeInTheDocument();
  });

  it("moves through the project setup steps", async () => {
    const user = userEvent.setup();
    renderApp("/app/acme/northstar/setup");
    expect(
      screen.getByRole("heading", { name: "品牌与来源" }),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "下一步" }));
    expect(
      screen.getByRole("heading", { name: "市场与目标" }),
    ).toBeInTheDocument();
  });

  it("shows a 404 page for unknown paths", () => {
    renderApp("/not-a-page");
    expect(
      screen.getByRole("heading", { name: "找不到这个页面" }),
    ).toBeInTheDocument();
  });
});
