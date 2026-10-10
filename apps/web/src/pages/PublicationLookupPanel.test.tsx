import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import {
  PublicationLookupPanel,
  safePublicUrl,
} from "./PublicationLookupPanel";

vi.mock("../auth/AuthProvider", () => ({
  useAuth: () => ({
    session: { user: { id: "user-1" }, operator: { id: "operator-1" } },
  }),
}));

const observation = (id: string, publicUrl: string | null = null) => ({
  execution_id: id,
  finding: "asset_observed",
  observed_at: "2026-10-01T00:00:00Z",
  received_at: "2026-10-01T00:01:00Z",
  error_code: null,
  public_url: publicUrl,
});

const page = (
  observations: ReturnType<typeof observation>[] = [],
  nextBefore: string | null = null,
  job: unknown = {
    query_count: 2,
    next_due_at: "2026-10-02T00:00:00Z",
    last_error_code: null,
    in_progress: false,
  },
) => ({
  target_id: "target-1",
  attempt_id: "attempt-1",
  job,
  observations,
  next_before: nextBefore,
});

function response(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function renderPanel(targetId = "target-1", projectId = "project-1") {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const panel = (target: string, project: string) => (
    <QueryClientProvider client={client}>
      <FluentProvider theme={webLightTheme}>
        <PublicationLookupPanel
          tenantId="tenant-1"
          projectId={project}
          targetId={target}
        />
      </FluentProvider>
    </QueryClientProvider>
  );
  const rendered = render(panel(targetId, projectId));
  return {
    ...rendered,
    changeScope: (target: string, project: string) =>
      rendered.rerender(panel(target, project)),
  };
}

afterEach(() => vi.unstubAllGlobals());

describe("publication lookup observation", () => {
  it("shows an initial read error and can retry without any write", async () => {
    const methods: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn((_input: RequestInfo | URL, init?: RequestInit) => {
        methods.push(init?.method ?? "GET");
        return Promise.resolve(
          methods.length === 1
            ? response(
                { code: "unavailable", message: "temporarily unavailable" },
                503,
              )
            : response(page([], null, null)),
        );
      }),
    );
    const user = userEvent.setup();
    renderPanel();
    expect(await screen.findByText("查回记录无法读取")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "重试" }));
    expect(
      await screen.findByText("暂无公开资产观察记录。"),
    ).toBeInTheDocument();
    expect(methods).toEqual(["GET", "GET"]);
  });

  it("loads older executions with scoped cursor and never treats an observed asset as send proof", async () => {
    const requests: URL[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn((input: RequestInfo | URL) => {
        const url = new URL(String(input), "http://localhost");
        requests.push(url);
        return Promise.resolve(
          response(
            url.searchParams.has("before")
              ? page([
                  observation("execution-newest", "javascript:alert(1)"),
                  observation("execution-older", "https://www.zhihu.com/p/123"),
                ])
              : page(
                  [observation("execution-newest", "javascript:alert(1)")],
                  "execution-newest",
                ),
          ),
        );
      }),
    );
    const user = userEvent.setup();
    renderPanel();
    expect(
      await screen.findByText(/已发现公开资产，原发送仍待核对/),
    ).toBeInTheDocument();
    expect(screen.getByText(/原发送结果待核对/)).toBeInTheDocument();
    expect(
      screen.queryByRole("link", { name: "查看公开资产" }),
    ).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "加载更早记录" }));
    expect(
      await screen.findByRole("link", { name: "查看公开资产" }),
    ).toHaveAttribute("href", "https://www.zhihu.com/p/123");
    expect(requests).toHaveLength(2);
    expect(screen.getAllByText(/已发现公开资产，原发送仍待核对/)).toHaveLength(
      2,
    );
    expect(requests[1].pathname).toBe(
      "/api/v1/projects/project-1/channel-targets/target-1/publication-lookup",
    );
    expect(requests[1].searchParams.get("before")).toBe("execution-newest");
    expect(requests[1].searchParams.get("tenant_id")).toBe("tenant-1");
    expect(requests[1].searchParams.get("project_id")).toBe("project-1");
    expect(
      screen.queryByRole("button", { name: /重发|审批/ }),
    ).not.toBeInTheDocument();
    expect(
      requests.every((item) => item.pathname.endsWith("/publication-lookup")),
    ).toBe(true);
  });

  it("shows unscheduled and empty states; refresh reads latest observations", async () => {
    let count = 0;
    vi.stubGlobal(
      "fetch",
      vi.fn(() =>
        Promise.resolve(
          response(
            ++count === 1
              ? page([], null, null)
              : page([observation("new")], null, null),
          ),
        ),
      ),
    );
    const user = userEvent.setup();
    renderPanel();
    expect(await screen.findByText(/尚未安排自动查回/)).toBeInTheDocument();
    expect(screen.getByText("暂无公开资产观察记录。")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "刷新查回" }));
    expect(
      await screen.findByText(/已发现公开资产，原发送仍待核对/),
    ).toBeInTheDocument();
    expect(count).toBe(2);
  });

  it("keeps earlier entries when a later page fails and allows retry", async () => {
    let attempts = 0;
    vi.stubGlobal(
      "fetch",
      vi.fn((input: RequestInfo | URL) => {
        const url = new URL(String(input), "http://localhost");
        if (!url.searchParams.has("before"))
          return Promise.resolve(
            response(page([observation("first")], "first")),
          );
        return Promise.resolve(
          ++attempts === 1
            ? response({ code: "unavailable", message: "temporary error" }, 503)
            : response(page([observation("second")])),
        );
      }),
    );
    const user = userEvent.setup();
    renderPanel();
    await screen.findByText(/已发现公开资产，原发送仍待核对/);
    await user.click(screen.getByRole("button", { name: "加载更早记录" }));
    expect(await screen.findByText("更早的记录无法读取")).toBeInTheDocument();
    expect(
      screen.getByText(/已发现公开资产，原发送仍待核对/),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "重试" }));
    await waitFor(() =>
      expect(
        screen.getAllByText(/已发现公开资产，原发送仍待核对/),
      ).toHaveLength(2),
    );
  });

  it("retains previous evidence and shows an error when background refresh fails", async () => {
    let count = 0;
    vi.stubGlobal(
      "fetch",
      vi.fn(() =>
        Promise.resolve(
          ++count === 1
            ? response(
                page([observation("first")], null, {
                  query_count: 1,
                  next_due_at: null,
                  last_error_code: "account_busy",
                  in_progress: false,
                }),
              )
            : response({ code: "unavailable", message: "refresh failed" }, 503),
        ),
      ),
    );
    const user = userEvent.setup();
    renderPanel();
    expect(
      await screen.findByText(/账号正忙，稍后自动查回/),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "刷新查回" }));
    expect(
      await screen.findByText("更新查回记录失败，仍显示已读取记录"),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/已发现公开资产，原发送仍待核对/),
    ).toBeInTheDocument();
  });

  it("isolates previous target and project responses after scope changes", async () => {
    let resolveOld: ((value: Response) => void) | undefined;
    vi.stubGlobal(
      "fetch",
      vi.fn((input: RequestInfo | URL) => {
        const url = new URL(String(input), "http://localhost");
        if (url.pathname.includes("/target-1/")) {
          return new Promise<Response>((resolve) => {
            resolveOld = resolve;
          });
        }
        return Promise.resolve(response(page([observation("new-target")])));
      }),
    );
    const view = renderPanel();
    await waitFor(() => expect(resolveOld).toBeDefined());
    view.changeScope("target-2", "project-2");
    expect(
      await screen.findByText(/已发现公开资产，原发送仍待核对/),
    ).toBeInTheDocument();
    resolveOld?.(response(page([observation("old-target")], null, null)));
    await waitFor(() =>
      expect(screen.queryByText(/尚未安排自动查回/)).not.toBeInTheDocument(),
    );
    expect(screen.getByText(/已查 2 次/)).toBeInTheDocument();
  });

  it("rejects script, credential-bearing, and non-web links", () => {
    expect(safePublicUrl("javascript:alert(1)")).toBeNull();
    expect(safePublicUrl("https://name:pass@example.org/asset")).toBeNull();
    expect(safePublicUrl("data:text/html,bad")).toBeNull();
    expect(safePublicUrl("https://example.org/asset")).toBeNull();
    expect(safePublicUrl("http://www.zhihu.com/p/123")).toBeNull();
    expect(
      safePublicUrl("https://www.zhihu.com/p/123?token=hidden"),
    ).toBeNull();
    expect(safePublicUrl("https://www.zhihu.com/p/123")).toBe(
      "https://www.zhihu.com/p/123",
    );
  });
});
