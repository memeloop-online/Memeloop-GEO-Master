import {
  afterAll,
  afterEach,
  beforeAll,
  describe,
  expect,
  it,
  vi,
} from "vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { AppRoutes } from "../app";
import { AuthProvider } from "../auth/AuthProvider";
import type { AuthSession } from "../auth/types";
import i18n from "../i18n";

const originalScrollTo = Object.getOwnPropertyDescriptor(
  HTMLElement.prototype,
  "scrollTo",
);
beforeAll(() => {
  // jsdom has no layout scrolling; populated native transcripts invoke it.
  Object.defineProperty(HTMLElement.prototype, "scrollTo", {
    configurable: true,
    value: vi.fn(),
  });
});
afterAll(() => {
  if (originalScrollTo) {
    Object.defineProperty(HTMLElement.prototype, "scrollTo", originalScrollTo);
  } else {
    Reflect.deleteProperty(HTMLElement.prototype, "scrollTo");
  }
});

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

afterEach(async () => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  await i18n.changeLanguage("zh-CN");
});

describe("P00 AI workbench routing", () => {
  it.each([
    ["failed", "这次回复未完成。请刷新查看最新状态，或编辑后发送新消息。"],
    ["cancelled", "这次运行已取消。你可以编辑后发送新消息。"],
  ])(
    "shows a persisted %s turn after loading the conversation",
    async (status, notice) => {
      const conversation = {
        id: "conversation-a",
        title: "对话",
        status: "active",
        revision: 1,
        created_at: "2026-09-19T00:00:00Z",
        updated_at: "2026-09-19T00:00:00Z",
      };
      const fetchMock = vi.fn(
        (request: RequestInfo | URL, _init?: RequestInit) => {
          const path = new URL(String(request), "http://localhost").pathname;
          if (path.endsWith("/auth/session"))
            return Promise.resolve(response(session));
          if (path.endsWith("/agent/conversations/conversation-a")) {
            return Promise.resolve(
              response({
                conversation,
                messages: [
                  {
                    id: "message-a",
                    conversation_id: conversation.id,
                    turn_id: "turn-a",
                    role: "user",
                    content: "Help with this task",
                    attachments: [],
                    metadata: {},
                    sequence: 1,
                    created_at: conversation.created_at,
                  },
                ],
                turns: [{ id: "turn-a", status }],
                runs: [
                  {
                    id: "run-a",
                    conversation_id: conversation.id,
                    turn_id: "turn-a",
                    status,
                    capability: { status: "available", runtime: "deno_core" },
                    error: status === "failed" ? { code: "internal" } : null,
                    cancel_version: 0,
                    created_at: conversation.created_at,
                    updated_at: conversation.updated_at,
                  },
                ],
              }),
            );
          }
          if (path.endsWith("/agent/conversations"))
            return Promise.resolve(
              response({ items: [conversation], next_cursor: null }),
            );
          return Promise.resolve(response({ items: [], next_cursor: null }));
        },
      );
      vi.stubGlobal("fetch", fetchMock);
      renderApp("/app/tenant-a/project-a/chat/conversation-a");
      expect(await screen.findByText(notice)).toBeInTheDocument();
      expect(screen.getByText("Help with this task")).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "取消运行" })).toBeNull();
      const before = fetchMock.mock.calls.length;
      await userEvent
        .setup()
        .click(screen.getByRole("button", { name: "重新加载对话" }));
      await waitFor(() =>
        expect(fetchMock.mock.calls.length).toBeGreaterThan(before),
      );
      expect(
        fetchMock.mock.calls.some(
          ([, init]) => (init as RequestInit | undefined)?.method === "POST",
        ),
      ).toBe(false);
      await act(async () => {
        await i18n.changeLanguage("en");
      });
      expect(
        screen.getByText(
          status === "failed"
            ? "This reply did not finish. Refresh for the latest status, or edit and send a new message."
            : "This run was cancelled. You can edit and send a new message.",
        ),
      ).toBeInTheDocument();
    },
  );

  it.each(["queued", "running"])(
    "keeps the native cancel action for a persisted %s run",
    async (status) => {
      const conversation = {
        id: "conversation-a",
        title: "对话",
        status: "active",
        revision: 1,
        created_at: "2026-09-19T00:00:00Z",
        updated_at: "2026-09-19T00:00:00Z",
      };
      let cancelled = false;
      const requests: Array<{ path: string; method?: string }> = [];
      vi.stubGlobal(
        "fetch",
        vi.fn((request: RequestInfo | URL, init?: RequestInit) => {
          const path = new URL(String(request), "http://localhost").pathname;
          requests.push({ path, method: init?.method });
          if (path.endsWith("/auth/session"))
            return Promise.resolve(response(session));
          if (path.endsWith("/agent/turns/turn-a/cancel")) {
            cancelled = true;
            return Promise.resolve(response({ status: "cancelled" }));
          }
          if (path.endsWith("/agent/conversations/conversation-a"))
            return Promise.resolve(
              response({
                conversation,
                messages: [],
                turns: [],
                runs: [
                  {
                    id: "run-a",
                    conversation_id: conversation.id,
                    turn_id: "turn-a",
                    status: cancelled ? "cancelled" : status,
                    capability: { status: "available", runtime: "deno_core" },
                    cancel_version: 0,
                    created_at: conversation.created_at,
                    updated_at: conversation.updated_at,
                  },
                ],
              }),
            );
          return Promise.resolve(
            response({ items: [conversation], next_cursor: null }),
          );
        }),
      );
      renderApp("/app/tenant-a/project-a/chat/conversation-a");
      expect(
        await screen.findByText(status === "queued" ? "等待运行" : "正在运行"),
      ).toBeInTheDocument();
      expect(
        screen.getByRole("button", { name: "取消运行" }),
      ).toBeInTheDocument();
      await userEvent
        .setup()
        .click(screen.getByRole("button", { name: "取消运行" }));
      await waitFor(() =>
        expect(screen.queryByRole("button", { name: "取消运行" })).toBeNull(),
      );
      expect(
        requests.filter(
          ({ path, method }) =>
            path.endsWith("/agent/turns/turn-a/cancel") && method === "POST",
        ),
      ).toHaveLength(1);
    },
  );

  it("reconciles a local accepted turn with its later persisted failure", async () => {
    const conversation = {
      id: "conversation-a",
      title: "对话",
      status: "active",
      revision: 1,
      created_at: "2026-09-19T00:00:00Z",
      updated_at: "2026-09-19T00:00:00Z",
    };
    let terminal = false;
    let stream:
      | { onmessage?: (event: { data: string; lastEventId: string }) => void }
      | undefined;
    vi.stubGlobal(
      "EventSource",
      class {
        onmessage?: (event: { data: string; lastEventId: string }) => void;
        constructor() {
          stream = this;
        }
        close() {}
      },
    );
    const fetchMock = vi.fn(
      (request: RequestInfo | URL, init?: RequestInit) => {
        const path = new URL(String(request), "http://localhost").pathname;
        if (path.endsWith("/auth/session"))
          return Promise.resolve(response(session));
        if (init?.method === "POST")
          return Promise.resolve(
            response({
              conversation_id: conversation.id,
              turn_id: "turn-a",
              run_id: "run-a",
              run_status: "queued",
              status: "accepted",
              events_url: "/events",
            }),
          );
        if (path.endsWith("/agent/conversations/conversation-a"))
          return Promise.resolve(
            response({
              conversation,
              messages: terminal
                ? [
                    {
                      id: "message-a",
                      conversation_id: conversation.id,
                      turn_id: "turn-a",
                      role: "user",
                      content: "One request",
                      attachments: [],
                      metadata: {},
                      sequence: 1,
                      created_at: conversation.created_at,
                    },
                  ]
                : [],
              turns: [],
              runs: terminal
                ? [
                    {
                      id: "run-a",
                      conversation_id: conversation.id,
                      turn_id: "turn-a",
                      status: "failed",
                      capability: { status: "available", runtime: "deno_core" },
                      error: { code: "internal" },
                      cancel_version: 0,
                      created_at: conversation.created_at,
                      updated_at: conversation.updated_at,
                    },
                  ]
                : [],
            }),
          );
        return Promise.resolve(
          response({ items: [conversation], next_cursor: null }),
        );
      },
    );
    vi.stubGlobal("fetch", fetchMock);
    renderApp("/app/tenant-a/project-a/chat/conversation-a");
    const user = userEvent.setup();
    await user.type(await screen.findByLabelText("输入任务"), "One request");
    await user.click(screen.getByRole("button", { name: "发送" }));
    expect(
      await screen.findByRole("button", { name: "取消运行" }),
    ).toBeInTheDocument();
    terminal = true;
    await act(async () => {
      stream?.onmessage?.({ data: "{}", lastEventId: "1" });
    });
    expect(
      await screen.findByText(
        "这次回复未完成。请刷新查看最新状态，或编辑后发送新消息。",
      ),
    ).toBeInTheDocument();
    await waitFor(() =>
      expect(screen.queryByRole("button", { name: "取消运行" })).toBeNull(),
    );
    await user.type(screen.getByLabelText("输入任务"), "Next message");
    expect(screen.getByRole("button", { name: "发送" })).toBeEnabled();
    expect(
      fetchMock.mock.calls.filter(
        ([, init]) => (init as RequestInit | undefined)?.method === "POST",
      ),
    ).toHaveLength(1);
  });

  it.each([3, 0, -1, "3", null])(
    "shows an omission notice only for a valid latest assistant count (%s)",
    async (count) => {
      const conversation = {
        id: "conversation-a",
        title: "历史范围",
        status: "active",
        revision: 1,
        created_at: "2026-09-19T00:00:00Z",
        updated_at: "2026-09-19T00:00:00Z",
      };
      vi.stubGlobal(
        "fetch",
        vi.fn((request: RequestInfo | URL) => {
          const path = new URL(String(request), "http://localhost").pathname;
          if (path.endsWith("/auth/session"))
            return Promise.resolve(response(session));
          if (path.endsWith("/agent/conversations/conversation-a")) {
            return Promise.resolve(
              response({
                conversation,
                messages: [
                  {
                    id: "answer-old",
                    conversation_id: conversation.id,
                    turn_id: "turn-old",
                    role: "assistant",
                    content: "Earlier answer",
                    sequence: 1,
                    metadata: { history_omitted_turns: 9 },
                    created_at: conversation.created_at,
                  },
                  {
                    id: "answer-new",
                    conversation_id: conversation.id,
                    turn_id: "turn-new",
                    role: "assistant",
                    content: "Latest answer",
                    sequence: 2,
                    metadata: { history_omitted_turns: count },
                    created_at: conversation.created_at,
                  },
                ],
                turns: [],
                runs: [],
              }),
            );
          }
          if (path.endsWith("/agent/conversations")) {
            return Promise.resolve(
              response({ items: [conversation], next_cursor: null }),
            );
          }
          return Promise.resolve(response({ items: [], next_cursor: null }));
        }),
      );
      renderApp("/app/tenant-a/project-a/chat/conversation-a");
      await screen.findByTestId("memeloop-agent-chat");
      if (count === 3) {
        expect(screen.getByLabelText("历史上下文范围")).toHaveTextContent(
          "3 轮已完成对话",
        );
      } else {
        expect(screen.queryByLabelText("历史上下文范围")).toBeNull();
      }
    },
  );

  it("makes P00 the first/default project destination and renders an honest empty state", async () => {
    const fetchMock = vi.fn(
      (request: RequestInfo | URL, _init?: RequestInit) => {
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
      },
    );
    vi.stubGlobal("fetch", fetchMock);

    renderApp("/app/tenant-a/project-a");

    expect(
      await screen.findByTestId("agent-workbench-page"),
    ).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "AI 工作台" })).toHaveAttribute(
      "href",
      "/app/tenant-a/project-a/chat",
    );
    expect(
      screen.getByRole("heading", { name: "从你的资料或想法开始" }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("输入任务")).toHaveAttribute(
      "placeholder",
      "提出问题、描述任务，或添加文件",
    );
    expect(
      screen.queryByRole("button", { name: "新建对话" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /^新建$/ }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByText("告诉 AI 你想完成的工作，也可以添加资料。"),
    ).toBeInTheDocument();
    expect(
      fetchMock.mock.calls.filter(
        ([, options]) =>
          (options as RequestInit | undefined)?.method === "POST",
      ),
    ).toHaveLength(0);
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
      screen.getByText("AI 服务未启用，请联系管理员完成配置。"),
    ).toBeInTheDocument();
    expect(screen.getByText("从你的资料或想法开始")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "添加文件" }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("输入任务")).toBeInTheDocument();
    expect(
      screen.getByText("可以在对话中逐步补充必要信息。"),
    ).toBeInTheDocument();
    await act(async () => {
      await i18n.changeLanguage("en");
    });
    expect(
      screen.getByText("Tell AI what you want to do, or add files."),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Enter a task")).toHaveAttribute(
      "placeholder",
      "Ask a question, describe a task, or add a file",
    );
    expect(
      screen.getByText(
        "AI service is not enabled. Contact your administrator to set it up.",
      ),
    ).toBeInTheDocument();
  });

  it.each(["conversation", "message"])(
    "creates the conversation only on first send and retains keys/input after %s failure",
    async (failure) => {
      const requests: Array<{ path: string; init?: RequestInit }> = [];
      let failed = false;
      const conversation = {
        id: "first-conversation",
        title: null,
        status: "active",
        revision: 1,
        created_at: "2026-09-19T00:00:00Z",
        updated_at: "2026-09-19T00:00:00Z",
      };
      vi.stubGlobal(
        "fetch",
        vi.fn((request: RequestInfo | URL, init?: RequestInit) => {
          const path = new URL(String(request), "http://localhost").pathname;
          requests.push({ path, init });
          if (path.endsWith("/auth/session"))
            return Promise.resolve(response(session));
          if (init?.method === "POST") {
            const isCreate = path.endsWith("/agent/conversations");
            if (
              !failed &&
              ((failure === "conversation" && isCreate) ||
                (failure === "message" && !isCreate))
            ) {
              failed = true;
              return Promise.resolve(
                response({ error: { code: "unavailable" } }, 503),
              );
            }
            return Promise.resolve(
              response(
                isCreate
                  ? conversation
                  : {
                      conversation_id: conversation.id,
                      turn_id: "turn-first",
                      run_status: "succeeded",
                    },
                201,
              ),
            );
          }
          if (path.endsWith(`/agent/conversations/${conversation.id}`))
            return Promise.resolve(
              response({
                conversation,
                messages: [],
                turns: [],
                runs: [],
              }),
            );
          return Promise.resolve(response({ items: [], next_cursor: null }));
        }),
      );
      renderApp("/app/tenant-a/project-a/chat");
      const input = await screen.findByLabelText("输入任务");
      expect(
        requests.filter(({ init }) => init?.method === "POST"),
      ).toHaveLength(0);
      const user = userEvent.setup();
      await user.type(input, "先整理我的产品资料");
      const send = screen.getByRole("button", { name: "发送" });
      fireEvent.click(send);
      fireEvent.click(send);
      await screen.findByText(/操作未完成。请稍后重试/);
      expect(screen.getByLabelText("输入任务")).toHaveValue(
        "先整理我的产品资料",
      );
      await user.click(screen.getByRole("button", { name: "发送" }));
      await waitFor(() =>
        expect(
          requests.some(
            ({ path, init }) =>
              path.endsWith("/agent/conversations/first-conversation") &&
              init?.method !== "POST",
          ),
        ).toBe(true),
      );
      const creates = requests.filter(
        ({ path, init }) =>
          path.endsWith("/agent/conversations") && init?.method === "POST",
      );
      const messages = requests.filter(
        ({ path, init }) =>
          path.endsWith("/messages") && init?.method === "POST",
      );
      expect(creates).toHaveLength(failure === "conversation" ? 2 : 1);
      expect(messages).toHaveLength(failure === "message" ? 2 : 1);
      const retried = failure === "conversation" ? creates : messages;
      expect(new Headers(retried[0].init?.headers).get("Idempotency-Key")).toBe(
        new Headers(retried[1].init?.headers).get("Idempotency-Key"),
      );
      expect(JSON.parse(String(messages.at(-1)?.init?.body)).content).toBe(
        "先整理我的产品资料",
      );
      expect(
        await screen.findByRole("heading", { name: /新对话 · 2026年9月19日/ }),
      ).toBeInTheDocument();
      await act(async () => {
        await i18n.changeLanguage("en");
      });
      expect(
        screen.getByRole("heading", {
          name: /New conversation · Sep 19, 2026/,
        }),
      ).toBeInTheDocument();
    },
  );

  it("keeps verified attachments after a partial failure, retries only the failed file, and submits references", async () => {
    const bytes = new Uint8Array([1, 2]);
    vi.stubGlobal("crypto", {
      subtle: {
        digest: vi.fn().mockResolvedValue(new Uint8Array(32).fill(0xab)),
      },
      randomUUID: (() => {
        let index = 0;
        return () => `uuid-${++index}`;
      })(),
    });
    const detail = {
      conversation: {
        id: "conversation-a",
        title: "附件任务",
        status: "active",
        revision: 1,
        created_at: "2026-09-19T00:00:00Z",
        updated_at: "2026-09-19T00:00:00Z",
      },
      messages: [],
      turns: [],
      runs: [],
    };
    let failedOnce = false;
    let messageFailedOnce = false;
    const requests: Array<{ url: string; init?: RequestInit }> = [];
    const fetchMock = vi.fn(
      (request: RequestInfo | URL, init?: RequestInit) => {
        const url = String(request);
        requests.push({ url, init });
        if (url.endsWith("/auth/session"))
          return Promise.resolve(response(session));
        if (url.includes("/projects?"))
          return Promise.resolve(response({ items: [], next_cursor: null }));
        if (url.includes("/agent/conversations/conversation-a/messages")) {
          if (!messageFailedOnce) {
            messageFailedOnce = true;
            return Promise.resolve(
              response({ code: "temporarily_unavailable" }, 503),
            );
          }
          return Promise.resolve(
            response({
              status: "accepted",
              conversation_id: "conversation-a",
              message_id: "message-a",
              turn_id: "turn-a",
              run_id: "run-a",
              events_url: "/events",
              run_status: "failed",
              error: { code: "capability_missing" },
            }),
          );
        }
        if (url.includes("/agent/conversations/conversation-a?"))
          return Promise.resolve(response(detail));
        if (url.includes("/agent/conversations?"))
          return Promise.resolve(
            response(
              init?.method === "POST"
                ? detail.conversation
                : { items: [], next_cursor: null },
            ),
          );
        if (
          url.includes("/agent/attachments/upload-sessions/") &&
          url.includes("/content?")
        ) {
          if (url.includes("/session-second/") && !failedOnce) {
            failedOnce = true;
            return Promise.resolve(response({ code: "upload_failed" }, 503));
          }
          return Promise.resolve(response({ upload_session_id: "uploaded" }));
        }
        if (
          url.includes("/agent/attachments/upload-sessions/") &&
          url.includes("/complete?")
        ) {
          const first = url.includes("/session-first/");
          return Promise.resolve(
            response({
              attachment_id: first ? "attachment-first" : "attachment-second",
              object_id: first ? "object-first" : "object-second",
              filename: first ? "first.txt" : "second.txt",
              media_type: "text/plain",
              size_bytes: 2,
              sha256: "ab".repeat(32),
              object_version: "1",
            }),
          );
        }
        if (url.includes("/agent/attachments/upload-sessions?")) {
          const body = JSON.parse(String(init?.body)) as { filename: string };
          return Promise.resolve(
            response(
              {
                upload_session_id:
                  body.filename === "first.txt"
                    ? "session-first"
                    : "session-second",
              },
              201,
            ),
          );
        }
        return Promise.resolve(response({}));
      },
    );
    vi.stubGlobal("fetch", fetchMock);
    renderApp("/app/tenant-a/project-a/chat");
    await screen.findByTestId("memeloop-agent-chat");
    expect(requests.filter(({ init }) => init?.method === "POST")).toHaveLength(
      0,
    );
    const fileInput = screen.getByTestId("agent-multi-file-input");
    const first = new File([bytes], "first.txt", { type: "text/plain" });
    const second = new File([bytes], "second.txt", { type: "text/plain" });
    for (const file of [first, second]) {
      Object.defineProperty(file, "arrayBuffer", {
        value: () => Promise.resolve(bytes.buffer),
      });
    }
    fireEvent.change(fileInput, { target: { files: [first, second] } });
    expect(
      screen.getByText(
        "上传的文件可在对话中使用。需要加入企业知识时，请告诉 AI。",
      ),
    ).toBeInTheDocument();
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "仅发送附件" }));
    expect(await screen.findByText(/部分附件上传失败/)).toBeInTheDocument();
    expect(
      screen.getByText(/first.txt · 已上传，可在对话中使用/),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "重试 second.txt" }),
    ).toBeInTheDocument();
    expect(
      requests.filter(({ url }) => url.includes("/messages?")),
    ).toHaveLength(0);

    await user.click(screen.getByRole("button", { name: "重试 second.txt" }));
    await screen.findByText(/second.txt · 已上传，可在对话中使用/);
    await act(async () => {
      await i18n.changeLanguage("en");
    });
    expect(
      screen.getByText(/first.txt · Uploaded · ready to use in chat/),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/second.txt · Uploaded · ready to use in chat/),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/Some attachments failed to upload/),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        "Uploaded files can be used in chat. Ask AI to add them to company knowledge when needed.",
      ),
    ).toBeInTheDocument();
    await act(async () => {
      await i18n.changeLanguage("zh-CN");
    });
    await user.click(screen.getByRole("button", { name: "仅发送附件" }));
    await waitFor(() =>
      expect(
        requests.filter(({ url }) => url.includes("/messages?")),
      ).toHaveLength(1),
    );
    expect(await screen.findByText(/消息发送未完成/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "仅发送附件" }));
    await waitFor(() =>
      expect(
        requests.filter(({ url }) => url.includes("/messages?")),
      ).toHaveLength(2),
    );
    const messages = requests.filter(({ url }) => url.includes("/messages?"));
    expect(new Headers(messages[0].init?.headers).get("Idempotency-Key")).toBe(
      new Headers(messages[1].init?.headers).get("Idempotency-Key"),
    );
    const message = messages[1];
    expect(JSON.parse(String(message.init?.body))).toEqual({
      content: "",
      attachments: [
        expect.objectContaining({ attachment_id: "attachment-first" }),
        expect.objectContaining({ attachment_id: "attachment-second" }),
      ],
    });
    expect(
      requests.filter(({ url }) => url.includes("/session-first/content?")),
    ).toHaveLength(1);
    expect(
      requests.filter(({ url }) => url.includes("/session-second/content?")),
    ).toHaveLength(2);
    expect(requests.some(({ url }) => url.includes("/knowledge/"))).toBe(false);
    expect(
      requests.filter(
        ({ url, init }) =>
          url.includes("/agent/conversations?") && init?.method === "POST",
      ),
    ).toHaveLength(1);
  });
});
