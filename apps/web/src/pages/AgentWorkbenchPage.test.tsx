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
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, useLocation } from "react-router-dom";
import type { ReactNode } from "react";
import { AppRoutes } from "../app";
import { AuthProvider } from "../auth/AuthProvider";
import type { AuthSession } from "../auth/types";
import i18n from "../i18n";
import "./AgentWorkbenchPage";

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

function renderApp(path: string, observer?: ReactNode) {
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
            {observer}
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
  it("renders saved assistant Markdown in native chat while preserving the composer and user text", async () => {
    const conversation = {
      id: "conversation-a",
      title: "Saved discussion",
      status: "active",
      revision: 1,
      created_at: "2026-09-19T00:00:00Z",
      updated_at: "2026-09-19T00:00:00Z",
    };
    const draft = "## Keep my [draft](https://example.org/draft)";
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
                  id: "question-a",
                  conversation_id: conversation.id,
                  turn_id: "turn-a",
                  role: "user",
                  content: draft,
                  sequence: 1,
                  created_at: conversation.created_at,
                },
                {
                  id: "answer-a",
                  conversation_id: conversation.id,
                  turn_id: "turn-a",
                  role: "assistant",
                  content:
                    "## Recommendation summary\n\n| Option | Evidence |\n| --- | --- |\n| First | [Source one](https://example.org/one) |\n| Second | [Source two](https://example.org/two) |\n| Third | [Source three](https://example.org/three) |\n\n[View project](/app/tenant-a/project-a) · [View measurements](/app/tenant-a/project-a/measurement) · [unsafe](javascript:alert%281%29)",
                  sequence: 2,
                  metadata: {
                    model: "test-model",
                    run_id: "run-a",
                    turn_id: "turn-a",
                    conversation_id: conversation.id,
                    history_omitted_turns: 0,
                  },
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
      },
    );
    vi.stubGlobal("fetch", fetchMock);

    renderApp("/app/tenant-a/project-a/chat/conversation-a");
    const chat = await screen.findByTestId("memeloop-agent-chat");
    expect(
      await within(chat).findByRole("heading", {
        name: "Recommendation summary",
        level: 2,
      }),
    ).toBeInTheDocument();
    const table = within(chat).getByRole("table");
    expect(within(table).getAllByRole("row")).toHaveLength(4);
    const links = within(table).getAllByRole("link");
    expect(links.map((link) => link.getAttribute("href"))).toEqual([
      "https://example.org/one",
      "https://example.org/two",
      "https://example.org/three",
    ]);
    for (const link of links) {
      expect(link).toHaveAttribute("target", "_blank");
      expect(link).toHaveAttribute("rel", "noopener noreferrer");
    }
    expect(
      within(chat).getByRole("link", { name: "View project" }),
    ).toHaveAttribute("href", "/app/tenant-a/project-a");
    const measurementLink = within(chat).getByRole("link", {
      name: "View measurements",
    });
    expect(measurementLink).toHaveAttribute(
      "href",
      "/app/tenant-a/project-a/measurement",
    );
    expect(measurementLink).not.toHaveAttribute("target");
    expect(within(chat).getByText("unsafe")).not.toHaveAttribute("href");
    expect(within(chat).getByText(draft)).toBeInTheDocument();
    const input = screen.getByLabelText("输入任务");
    await userEvent.setup().type(input, "## Editable draft");
    expect(input).toHaveValue("## Editable draft");
    expect(
      screen.getByRole("button", { name: "添加文件" }),
    ).toBeInTheDocument();
    expect(
      fetchMock.mock.calls.filter(([, init]) => init?.method === "POST"),
    ).toHaveLength(0);
  });

  it.each(["zh-CN", "en"])(
    "offers editable measurement examples without overwriting or sending drafts (%s)",
    async (language) => {
      await i18n.changeLanguage(language);
      const requests: Array<{ path: string; init?: RequestInit }> = [];
      vi.stubGlobal(
        "fetch",
        vi.fn((request: RequestInfo | URL, init?: RequestInit) => {
          const path = new URL(String(request), "http://localhost").pathname;
          requests.push({ path, init });
          return Promise.resolve(
            response(
              path.endsWith("/auth/session")
                ? session
                : { items: [], next_cursor: null },
            ),
          );
        }),
      );
      renderApp("/app/tenant-a/project-a/chat");
      const input = await screen.findByLabelText(i18n.t("chatWorkbench.input"));
      const user = userEvent.setup();
      const guidance = (key: string) =>
        i18n.t(key, { ns: "chatMeasurementGuidance" });
      expect(
        screen.getByRole("heading", { name: guidance("title") }),
      ).toBeInTheDocument();
      for (const kind of ["product", "shop", "perception", "sources"]) {
        await user.clear(input);
        await user.click(
          screen.getByRole("button", {
            name: `${guidance(`examples.${kind}.title`)} ${guidance(`examples.${kind}.detail`)}`,
          }),
        );
        expect(input).toHaveValue(guidance(`examples.${kind}.draft`));
        expect(input).toHaveFocus();
        await user.clear(input);
        await user.type(input, "My edited draft");
        await user.click(
          screen.getByRole("button", {
            name: `${guidance(`examples.${kind}.title`)} ${guidance(`examples.${kind}.detail`)}`,
          }),
        );
        expect(input).toHaveValue("My edited draft");
        expect(input).toHaveFocus();
        expect(screen.getByText(guidance("preserved"))).toBeInTheDocument();
      }
      expect(
        requests.filter(({ init }) => init?.method === "POST"),
      ).toHaveLength(0);
    },
  );

  it.each(["plan", "invalid", "mixed"])(
    "handles a measurement-plan handoff without starting a conversation: %s",
    async (reference) => {
      const planId = "11111111-2222-4333-8444-555555555555";
      const requests: Array<{ init?: RequestInit }> = [];
      vi.stubGlobal(
        "fetch",
        vi.fn((request: RequestInfo | URL, init?: RequestInit) => {
          requests.push({ init });
          return Promise.resolve(
            response(
              String(request).includes("/auth/session")
                ? session
                : { items: [], next_cursor: null },
            ),
          );
        }),
      );
      renderApp(
        `/app/tenant-a/project-a/chat?measurementPlan=${reference === "invalid" ? "invalid" : planId}${reference === "mixed" ? `&measurementReport=${planId}` : ""}`,
      );
      const input = await screen.findByLabelText("输入任务");
      await waitFor(() =>
        reference === "plan"
          ? expect((input as HTMLTextAreaElement).value).toContain(planId)
          : expect(input).toHaveValue(""),
      );
      expect(
        requests.filter(({ init }) => init?.method === "POST"),
      ).toHaveLength(0);
    },
  );

  it.each([
    "unsent",
    "selected-question",
    "selected-recommendation",
    "invalid-question",
    "question-only",
    "invalid-reference",
    "saved-conversation",
  ])(
    "handles a report handoff in the native composer: %s",
    async (destination) => {
      const reportId = "11111111-2222-4333-8444-555555555555";
      const questionTargetId = "66666666-7777-4888-8999-000000000000";
      const conversation = {
        id: "synthetic-conversation",
        title: "Synthetic saved conversation",
        status: "active",
        revision: 1,
        created_at: "2026-09-19T00:00:00Z",
        updated_at: "2026-09-19T00:00:00Z",
      };
      const requests: Array<{ path: string; init?: RequestInit }> = [];
      vi.stubGlobal(
        "fetch",
        vi.fn((request: RequestInfo | URL, init?: RequestInit) => {
          const path = new URL(String(request), "http://localhost").pathname;
          requests.push({ path, init });
          if (path.endsWith("/auth/session"))
            return Promise.resolve(response(session));
          if (path.endsWith(`/agent/conversations/${conversation.id}`))
            return Promise.resolve(
              response({ conversation, messages: [], turns: [], runs: [] }),
            );
          if (init?.method === "POST")
            return Promise.resolve(
              response(
                path.endsWith("/agent/conversations")
                  ? conversation
                  : {
                      conversation_id: conversation.id,
                      turn_id: "synthetic-turn",
                      run_status: "succeeded",
                    },
                201,
              ),
            );
          return Promise.resolve(response({ items: [], next_cursor: null }));
        }),
      );
      function LocationProbe() {
        const { pathname, search } = useLocation();
        return <output data-testid="chat-location">{pathname + search}</output>;
      }
      const chatPath = "/app/tenant-a/project-a/chat";
      renderApp(
        `${chatPath}${destination === "saved-conversation" ? `/${conversation.id}` : ""}?${destination === "question-only" ? "" : `measurementReport=${destination === "invalid-reference" ? "invalid" : reportId}&`}${destination === "unsent" ? "" : `measurementQuestionTarget=${destination === "invalid-question" ? "invalid" : questionTargetId}`}${destination === "selected-recommendation" ? "&measurementAttempt=aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee&measurementRecommendationKind=product&measurementRecommendationOrder=3" : ""}`,
        <LocationProbe />,
      );
      const input = await screen.findByLabelText("输入任务");
      await waitFor(() => {
        if (
          ["unsent", "selected-question", "selected-recommendation"].includes(
            destination,
          )
        ) {
          expect((input as HTMLTextAreaElement).value).toContain(reportId);
          if (destination === "selected-question") {
            expect((input as HTMLTextAreaElement).value).toContain(
              `所选问题（样本编号：${questionTargetId}）`,
            );
          } else if (destination === "unsent") {
            expect((input as HTMLTextAreaElement).value).not.toContain(
              questionTargetId,
            );
          }
          if (destination === "selected-recommendation") {
            expect((input as HTMLTextAreaElement).value).toContain(
              `target_id=${questionTargetId}`,
            );
            expect((input as HTMLTextAreaElement).value).toContain(
              "mention_order=3",
            );
            expect((input as HTMLTextAreaElement).value).toContain(
              "attempt_id=aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
            );
          }
        } else {
          expect(input).toHaveValue("");
        }
      });
      expect(
        requests.filter(({ init }) => init?.method === "POST"),
      ).toHaveLength(0);
      expect(
        requests.some(({ path }) => path.includes("measurement-reports")),
      ).toBe(false);
      if (
        !["unsent", "selected-question", "selected-recommendation"].includes(
          destination,
        )
      )
        return;

      const user = userEvent.setup();
      await user.clear(input);
      await user.type(input, `请只分析报告 ${reportId} 的推荐理由`);
      await user.click(screen.getByRole("button", { name: "发送" }));
      await waitFor(() =>
        expect(screen.getByTestId("chat-location")).toHaveTextContent(
          `${chatPath}/${conversation.id}`,
        ),
      );
      expect(screen.getByTestId("chat-location")).not.toHaveTextContent("?");
      const creates = requests.filter(
        ({ path, init }) =>
          path.endsWith("/agent/conversations") && init?.method === "POST",
      );
      const messages = requests.filter(
        ({ path, init }) =>
          path.endsWith("/messages") && init?.method === "POST",
      );
      expect(creates).toHaveLength(1);
      expect(messages).toHaveLength(1);
      expect(JSON.parse(String(messages[0].init?.body)).content).toBe(
        `请只分析报告 ${reportId} 的推荐理由`,
      );
    },
  );

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
      await act(async () => {
        await import("./AgentWorkbenchPage");
      });
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
      screen.getByRole("heading", {
        name: "客户问 AI 时，会推荐你的产品或店铺吗？",
      }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("输入任务")).toHaveAttribute(
      "placeholder",
      "输入你的产品、店铺或想测量的问题…",
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
    expect(
      screen.getByText("客户问 AI 时，会推荐你的产品或店铺吗？"),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "添加文件" }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("输入任务")).toBeInTheDocument();
    expect(
      screen.getByText("选择示例，修改后发送；也可以直接提问。"),
    ).toBeInTheDocument();
    await act(async () => {
      await i18n.changeLanguage("en");
    });
    expect(
      screen.getByText("Tell AI what you want to do, or add files."),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Enter a task")).toHaveAttribute(
      "placeholder",
      "Enter your product, store, or a question to measure…",
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
