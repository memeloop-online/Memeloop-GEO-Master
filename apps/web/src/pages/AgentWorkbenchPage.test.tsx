import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { AppRoutes } from "../app";
import { AuthProvider } from "../auth/AuthProvider";
import type { AuthSession } from "../auth/types";

const session: AuthSession = {
  user: {
    id: "user-a",
    login_name: "demo@localhost",
    display_name: "Local Demo",
  },
  operator: { id: "operator-a", slug: "memeloop", display_name: "模因循环" },
  memberships: [
    {
      tenant_id: "tenant-a",
      tenant_slug: "northstar",
      tenant_display_name: "Northstar",
      role: "tenant_admin",
    },
  ],
  expires_at: "2026-09-19T08:00:00Z",
  csrf_token: "csrf-a",
};

function response(body: unknown, status = 200) {
  return new Response(body === undefined ? null : JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function renderApp(path: string) {
  return render(
    <FluentProvider theme={webLightTheme}>
      <QueryClientProvider
        client={
          new QueryClient({
            defaultOptions: { queries: { retry: false } },
          })
        }
      >
        <AuthProvider>
          <MemoryRouter initialEntries={[path]}>
            <AppRoutes />
          </MemoryRouter>
        </AuthProvider>
      </QueryClientProvider>
    </FluentProvider>,
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("P00 AI workbench routing", () => {
  it("makes P00 the first/default project destination and renders an honest empty state", async () => {
    const fetchMock = vi.fn((request: RequestInfo | URL) => {
      const url = new URL(String(request), "http://localhost");
      if (url.pathname.endsWith("/auth/session")) {
        return Promise.resolve(response(session));
      }
      if (url.pathname.endsWith("/projects")) {
        return Promise.resolve(response({ items: [], next_cursor: null }));
      }
      if (url.pathname.endsWith("/agent/conversations")) {
        return Promise.resolve(response({ items: [], next_cursor: null }));
      }
      return Promise.resolve(response({}));
    });
    vi.stubGlobal("fetch", fetchMock);

    renderApp("/app/tenant-a/project-a");

    expect(
      await screen.findByTestId("agent-workbench-page"),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: "P00 · AI 工作台" }),
    ).toHaveAttribute("href", "/app/tenant-a/project-a/chat");
    expect(
      screen.getByRole("heading", { name: "从一个项目任务开始" }),
    ).toBeInTheDocument();
    expect(screen.getByText(/这里不会生成演示回复。/)).toBeInTheDocument();
  });

  it("renders the MemeLoop chat shell and retains a missing-runtime notice", async () => {
    const fetchMock = vi.fn((request: RequestInfo | URL) => {
      const url = new URL(String(request), "http://localhost");
      if (url.pathname.endsWith("/auth/session")) {
        return Promise.resolve(response(session));
      }
      if (url.pathname.endsWith("/projects")) {
        return Promise.resolve(response({ items: [], next_cursor: null }));
      }
      if (url.pathname.endsWith("/agent/conversations/conversation-a")) {
        return Promise.resolve(
          response({
            conversation: {
              id: "conversation-a",
              title: "检查运行时",
              status: "active",
              revision: 1,
              created_at: "2026-09-19T00:00:00Z",
              updated_at: "2026-09-19T00:00:00Z",
            },
            messages: [],
            turns: [
              {
                id: "turn-a",
                conversation_id: "conversation-a",
                root_message_id: "message-a",
                status: "failed",
                cancel_version: 0,
                created_at: "2026-09-19T00:00:00Z",
                updated_at: "2026-09-19T00:00:00Z",
              },
            ],
            runs: [
              {
                id: "run-a",
                conversation_id: "conversation-a",
                turn_id: "turn-a",
                status: "failed",
                capability: {
                  status: "missing",
                  runtime: "deno_core",
                  reason: "embedded JavaScript runtime is not configured",
                },
                cancel_version: 0,
                created_at: "2026-09-19T00:00:00Z",
                updated_at: "2026-09-19T00:00:00Z",
              },
            ],
          }),
        );
      }
      if (url.pathname.endsWith("/agent/conversations")) {
        return Promise.resolve(
          response({
            items: [
              {
                id: "conversation-a",
                title: "检查运行时",
                status: "active",
                revision: 1,
                created_at: "2026-09-19T00:00:00Z",
                updated_at: "2026-09-19T00:00:00Z",
              },
            ],
            next_cursor: null,
          }),
        );
      }
      return Promise.resolve(response({}));
    });
    vi.stubGlobal("fetch", fetchMock);

    renderApp("/app/tenant-a/project-a/chat/conversation-a");

    expect(
      await screen.findByTestId("memeloop-agent-chat"),
    ).toBeInTheDocument();
    expect(
      screen.getByText("Rust JS Agent Runtime 尚未配置，本次未生成 AI 回复。"),
    ).toBeInTheDocument();
    expect(screen.getByText("此对话还没有消息")).toBeInTheDocument();
  });
});
