import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import {
  MemoryRouter,
  Route,
  Routes,
  useLocation,
  useNavigate,
} from "react-router-dom";
import { AuthProvider } from "../auth/AuthProvider";
import { setCsrfToken, setUnauthorizedHandler } from "../api/client";
import i18n from "../i18n";
import { QuestionSetsPage } from "./QuestionSetsPage";

const version = {
  id: "version-1",
  question_set_id: "set-1",
  revision: 1,
  parent_version_id: null,
  name: "基础问题",
  questions: [
    {
      id: "revision-1",
      question_id: "question-1",
      text: "怎样选型？",
      intent: "general",
      product_refs: [],
      market: "CN",
      language: "zh-CN",
      source: { kind: "user_provided" },
      weight: 1,
      purpose: "frozen_evaluation",
      split_policy_version: "project_registry_nfkc_v1",
    },
  ],
  optimization_count: 0,
  evaluation_count: 1,
  split_policy_version: "project_registry_nfkc_v1",
  content_hash: "hash-1",
  created_at: "2026-10-01T00:00:00Z",
};
const setSummary = {
  id: "set-1",
  name: "基础问题",
  current_version_id: version.id,
  current_revision: 1,
  question_count: 1,
  optimization_count: 0,
  evaluation_count: 1,
};
const reply = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });

function mockApi({
  existing = false,
  role = "tenant_admin",
  conflict = false,
  listError = false,
  createErrorOnce = false,
  sourceKinds,
}: {
  existing?: boolean;
  role?: string;
  conflict?: boolean;
  listError?: boolean;
  createErrorOnce?: boolean;
  sourceKinds?: string[];
} = {}) {
  const calls: {
    path: string;
    method: string;
    body: unknown;
    headers: Headers;
  }[] = [];
  let exists = existing;
  let rejectCreate = createErrorOnce;
  const initialQuestions = sourceKinds
    ? sourceKinds.map((kind, index) => ({
        ...version.questions[0],
        id: `revision-${index + 1}`,
        question_id: `question-${index + 1}`,
        text: `来源问题 ${index + 1}`,
        source: { kind },
        purpose: index === 0 ? "optimization" : "frozen_evaluation",
      }))
    : version.questions;
  let current = {
    ...version,
    questions: initialQuestions,
    optimization_count: sourceKinds ? (sourceKinds.length ? 1 : 0) : 0,
    evaluation_count: sourceKinds
      ? Math.max(sourceKinds.length - 1, 0)
      : version.evaluation_count,
    parent_version_id: null as string | null,
  };
  vi.stubGlobal(
    "fetch",
    vi.fn((request: RequestInfo | URL, init?: RequestInit) => {
      const path = new URL(String(request), "http://localhost").pathname;
      const method = init?.method ?? "GET";
      const body = init?.body ? JSON.parse(String(init.body)) : null;
      calls.push({ path, method, body, headers: new Headers(init?.headers) });
      if (path.endsWith("/serp-capabilities"))
        return Promise.resolve(reply([]));
      if (path.endsWith("/serp-measurements"))
        return Promise.resolve(reply({ items: [], next_after: null }));
      if (path.endsWith("/auth/session"))
        return Promise.resolve(
          reply({
            user: {
              id: "user-1",
              login_name: "user@example.test",
              display_name: "User",
            },
            operator: {
              id: "operator-1",
              slug: "operator",
              display_name: "Operator",
            },
            memberships: [
              {
                tenant_id: "tenant-1",
                tenant_slug: "tenant",
                tenant_display_name: "Tenant",
                role,
              },
            ],
            expires_at: "2026-10-01T00:00:00Z",
            csrf_token: "csrf-test",
          }),
        );
      if (path.endsWith("/projects/project-1/question-sets")) {
        if (method === "POST") {
          if (rejectCreate) {
            rejectCreate = false;
            return Promise.resolve(
              reply({ code: "unavailable", message: "Connection lost" }, 503),
            );
          }
          exists = true;
          current = {
            ...version,
            name: body.name,
            questions: body.questions.map(
              (item: (typeof version.questions)[number], index: number) => ({
                ...version.questions[0],
                ...item,
                id: `revision-${index + 1}`,
                question_id: `question-${index + 1}`,
              }),
            ),
          };
          return Promise.resolve(reply(current));
        }
        if (listError)
          return Promise.resolve(
            reply({ code: "internal", message: "Unavailable" }, 503),
          );
        return Promise.resolve(
          reply({
            items: exists
              ? [
                  {
                    ...setSummary,
                    name: current.name,
                    current_revision: current.revision,
                    current_version_id: current.id,
                    question_count: current.questions.length,
                  },
                ]
              : [],
            next_cursor: null,
          }),
        );
      }
      if (path.endsWith("/question-sets/set-1/versions")) {
        if (method === "POST") {
          if (conflict)
            return Promise.resolve(
              reply({ code: "conflict", message: "stale base" }, 409),
            );
          current = {
            ...current,
            id: "version-2",
            revision: 2,
            parent_version_id: "version-1",
            name: body.name,
            questions: body.questions.map(
              (item: (typeof version.questions)[number], index: number) => ({
                ...version.questions[0],
                ...item,
                id: `revision-next-${index + 1}`,
                question_id: item.question_id ?? `question-new-${index + 1}`,
                purpose: index === 0 ? "frozen_evaluation" : "optimization",
              }),
            ),
            optimization_count: 1,
          };
          return Promise.resolve(reply(current));
        }
        return Promise.resolve(
          reply({
            items: [current, version]
              .filter(
                (item, index, array) =>
                  array.findIndex((other) => other.id === item.id) === index,
              )
              .map(
                ({
                  questions: _questions,
                  content_hash: _hash,
                  ...summary
                }) => ({
                  ...summary,
                  question_count: current.questions.length,
                }),
              ),
            next_cursor: null,
          }),
        );
      }
      if (path.endsWith("/question-sets/set-1/versions/version-1"))
        return Promise.resolve(
          reply(current.revision === 1 ? current : version),
        );
      if (path.endsWith("/question-sets/set-1/versions/version-2"))
        return Promise.resolve(reply(current));
      return Promise.resolve(
        reply({ code: "not_found", message: "missing" }, 404),
      );
    }),
  );
  return calls;
}

function NavigationState() {
  const location = useLocation();
  const navigate = useNavigate();
  return (
    <>
      <output data-testid="measurement-url">{location.search}</output>
      <button onClick={() => navigate(-1)}>浏览器返回</button>
    </>
  );
}

function renderPage(initialEntry = "/app/tenant-1/project-1/measurement") {
  return render(
    <QueryClientProvider
      client={
        new QueryClient({
          defaultOptions: {
            queries: { retry: false },
            mutations: { retry: false },
          },
        })
      }
    >
      <FluentProvider theme={webLightTheme}>
        <AuthProvider>
          <MemoryRouter initialEntries={[initialEntry]}>
            <Routes>
              <Route
                path="/app/:tenantId/:projectId/measurement"
                element={
                  <>
                    <QuestionSetsPage />
                    <NavigationState />
                  </>
                }
              />
            </Routes>
          </MemoryRouter>
        </AuthProvider>
      </FluentProvider>
    </QueryClientProvider>,
  );
}

afterEach(async () => {
  sessionStorage.clear();
  vi.unstubAllGlobals();
  setCsrfToken(undefined);
  setUnauthorizedHandler(undefined);
  await i18n.changeLanguage("zh-CN");
});

describe("P13 versioned question sets", () => {
  it.each([
    ["zh-CN", "传统搜索"],
    ["en", "Web search"],
  ])(
    "opens the read-only search tab deep link in %s without creating a measurement",
    async (language, label) => {
      await i18n.changeLanguage(language);
      const calls = mockApi();
      renderPage("/app/tenant-1/project-1/measurement?tab=search");
      expect(await screen.findByRole("tab", { name: label })).toHaveAttribute(
        "aria-selected",
        "true",
      );
      const panel = screen.getByRole("tabpanel", { name: label });
      expect(
        within(panel).getByRole("heading", {
          name: i18n.t("title", { ns: "serp" }),
        }),
      ).toBeInTheDocument();
      expect(
        await within(panel).findByText(i18n.t("unavailable", { ns: "serp" })),
      ).toBeInTheDocument();
      expect(
        calls
          .filter((call) => call.path.includes("/serp-"))
          .every((call) => call.method === "GET"),
      ).toBe(true);
    },
  );

  it("keeps search input across tabs and navigates with the existing browser history", async () => {
    mockApi();
    renderPage();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("tab", { name: "传统搜索" }));
    expect(screen.getByTestId("measurement-url")).toHaveTextContent(
      "tab=search",
    );
    const panel = screen.getByRole("tabpanel", { name: "传统搜索" });
    await user.type(
      within(panel).getByRole("textbox", { name: /关键词/ }),
      "generic topic",
    );
    await user.click(screen.getByRole("tab", { name: "问题集" }));
    await user.click(screen.getByRole("button", { name: "浏览器返回" }));
    expect(screen.getByRole("tab", { name: "传统搜索" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(
      within(screen.getByRole("tabpanel", { name: "传统搜索" })).getByRole(
        "textbox",
        { name: /关键词/ },
      ),
    ).toHaveValue("generic topic");
  });

  it("creates a named immutable version from pasted lines and edits into a second version", async () => {
    const calls = mockApi();
    const user = userEvent.setup();
    renderPage();
    await user.click(screen.getByRole("tab", { name: "问题集" }));
    expect(await screen.findByText("尚无问题集")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "新建问题集" }));
    await user.type(screen.getByLabelText("问题集名称"), "基础问题");
    await user.type(
      screen.getByLabelText("每行一个问题"),
      "怎样选型？{enter}如何维护？",
    );
    await user.click(screen.getByRole("button", { name: "创建并保存问题集" }));
    expect(await screen.findByText(/冻结评估 1/)).toBeInTheDocument();
    const creation = calls.find(
      (call) =>
        call.method === "POST" &&
        call.path.endsWith("/projects/project-1/question-sets"),
    );
    expect(creation?.body).toMatchObject({
      name: "基础问题",
      questions: [
        {
          text: "怎样选型？",
          source: { kind: "user_provided" },
          product_refs: [],
          weight: 1,
        },
        {
          text: "如何维护？",
          source: { kind: "user_provided" },
          product_refs: [],
          weight: 1,
        },
      ],
    });
    expect(creation?.headers.get("Idempotency-Key")).toBe(
      (creation?.body as { idempotency_key: string }).idempotency_key,
    );
    await user.click(
      await screen.findByRole("button", { name: "基于当前版本修订" }),
    );
    await user.click(screen.getByRole("button", { name: "添加问题" }));
    await user.type(screen.getByLabelText("问题 3"), "有哪些规格？");
    await user.click(screen.getByRole("button", { name: "保存为新版本" }));
    expect(
      await screen.findByRole("heading", { name: "基础问题 · v2" }),
    ).toBeInTheDocument();
    const revision = calls.find(
      (call) =>
        call.method === "POST" &&
        call.path.endsWith("/question-sets/set-1/versions"),
    );
    expect(revision?.body).toMatchObject({
      base_version_id: "version-1",
      questions: [
        { question_id: "question-1", text: "怎样选型？" },
        { question_id: "question-2", text: "如何维护？" },
        { text: "有哪些规格？" },
      ],
    });
  });

  it("keeps the edit draft and optimistic base when another writer changed the version", async () => {
    const calls = mockApi({ existing: true, conflict: true });
    const user = userEvent.setup();
    renderPage();
    await user.click(screen.getByRole("tab", { name: "问题集" }));
    await user.selectOptions(
      await screen.findByLabelText("选择问题集"),
      "set-1",
    );
    await user.click(
      await screen.findByRole("button", { name: "基于当前版本修订" }),
    );
    await user.clear(screen.getByLabelText("问题 1"));
    await user.type(screen.getByLabelText("问题 1"), "保留我的草稿");
    await user.click(screen.getByRole("button", { name: "保存为新版本" }));
    expect(
      await screen.findByText(/版本已变化或问题身份冲突；草稿已保留/),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("问题 1")).toHaveValue("保留我的草稿");
    expect(
      (
        calls.find((call) => call.method === "POST")?.body as {
          base_version_id: string;
        }
      ).base_version_id,
    ).toBe("version-1");
  });

  it("keeps per-question details collapsed until requested and saves edited metadata", async () => {
    const calls = mockApi({ existing: true });
    const user = userEvent.setup();
    renderPage("/app/tenant-1/project-1/measurement?tab=sets&set=set-1");
    await screen.findByRole("heading", { name: "基础问题 · v1" });
    expect(
      screen.queryByText(/project_registry_nfkc_v1/),
    ).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "基于当前版本修订" }));
    const questionRow = screen
      .getByLabelText("问题 1")
      .closest(".question-sets-edit-row") as HTMLElement;
    const details = within(questionRow)
      .getByText("更多设置")
      .closest("details") as HTMLDetailsElement;
    expect(details).not.toHaveAttribute("open");
    expect(
      within(details).getByRole("textbox", { name: "问题 1 意图" }),
    ).toHaveValue("general");
    await user.click(within(questionRow).getByText("更多设置"));
    await user.clear(screen.getByRole("textbox", { name: "问题 1 意图" }));
    await user.type(
      screen.getByRole("textbox", { name: "问题 1 意图" }),
      "compare",
    );
    await user.click(within(questionRow).getByText("更多设置"));
    expect(details).not.toHaveAttribute("open");
    await user.click(screen.getByRole("button", { name: "保存为新版本" }));
    await screen.findByRole("heading", { name: "基础问题 · v2" });
    const revision = calls.find(
      ({ method, path }) =>
        method === "POST" && path.endsWith("/question-sets/set-1/versions"),
    );
    expect(revision?.body).toMatchObject({
      questions: [
        {
          text: "怎样选型？",
          intent: "compare",
          market: "CN",
          language: "zh-CN",
          weight: 1,
        },
      ],
    });
  });

  it("supports a read-only project and recoverable list failure", async () => {
    mockApi({ existing: true, role: "viewer" });
    renderPage();
    await userEvent.setup().click(screen.getByRole("tab", { name: "问题集" }));
    expect(await screen.findByText(/当前角色为只读/)).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "创建并保存问题集" }),
    ).not.toBeInTheDocument();
    const user = userEvent.setup();
    await user.selectOptions(
      await screen.findByLabelText("选择问题集"),
      "set-1",
    );
    expect(await screen.findByText(/当前版本没有优化问题/)).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "基于当前版本修订" }),
    ).not.toBeInTheDocument();
  });

  it("shows a retriable error when list access fails", async () => {
    const calls = mockApi({ listError: true });
    renderPage();
    await userEvent.setup().click(screen.getByRole("tab", { name: "问题集" }));
    expect(await screen.findByText("问题集无法读取")).toBeInTheDocument();
    expect(
      within(
        screen.getByText("问题集无法读取").closest(".fui-MessageBar")!,
      ).getByRole("button", { name: /重试/ }),
    ).toBeInTheDocument();
    expect(
      calls.filter(
        (item) => item.path.endsWith("/question-sets") && item.method === "GET",
      ),
    ).toHaveLength(1);
  });

  it("opens a version deep link, follows browser history, and keeps an unsaved revision across tabs and version browsing", async () => {
    mockApi({ existing: true });
    const user = userEvent.setup();
    renderPage(
      "/app/tenant-1/project-1/measurement?tab=sets&set=set-1&version=version-1",
    );
    await screen.findByRole("heading", { name: "基础问题 · v1" });
    await user.click(screen.getByRole("button", { name: "基于当前版本修订" }));
    await user.clear(screen.getByLabelText("问题 1"));
    await user.type(screen.getByLabelText("问题 1"), "未保存的修订");
    await user.click(screen.getByRole("tab", { name: "信源洞察" }));
    expect(screen.getByTestId("measurement-url")).toHaveTextContent(
      "tab=insights",
    );
    await user.click(screen.getByRole("button", { name: "浏览器返回" }));
    expect(screen.getByLabelText("问题 1")).toHaveValue("未保存的修订");
    await user.click(screen.getByRole("tab", { name: "开始测量" }));
    await user.click(screen.getByRole("tab", { name: "问题集" }));
    expect(screen.getByLabelText("问题 1")).toHaveValue("未保存的修订");
    expect(screen.getByTestId("measurement-url")).toHaveTextContent(
      "set=set-1",
    );
  });

  it("holds a new-set draft when tabs switch and opens creation only on request", async () => {
    mockApi();
    const user = userEvent.setup();
    renderPage();
    expect(screen.queryByLabelText("问题集名称")).not.toBeInTheDocument();
    await user.click(screen.getByRole("tab", { name: "问题集" }));
    await user.click(screen.getByRole("button", { name: "新建问题集" }));
    await user.type(screen.getByLabelText("问题集名称"), "未保存的问题集");
    await user.click(screen.getByRole("button", { name: "收起新建" }));
    await user.click(screen.getByRole("button", { name: "新建问题集" }));
    expect(screen.getByLabelText("问题集名称")).toHaveValue("未保存的问题集");
    await user.click(screen.getByRole("tab", { name: "测量记录" }));
    await user.click(screen.getByRole("tab", { name: "问题集" }));
    expect(screen.getByLabelText("问题集名称")).toHaveValue("未保存的问题集");
  });

  it("uses keyboard-accessible tabs and keeps the starting view focused on measuring", async () => {
    mockApi();
    const user = userEvent.setup();
    renderPage();
    expect(screen.getByRole("tab", { name: "开始测量" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(
      await screen.findByRole("textbox", { name: "要测量的问题" }),
    ).toBeInTheDocument();
    screen.getByRole("tab", { name: "测量记录" }).focus();
    await user.keyboard("{Enter}");
    expect(screen.getByRole("tab", { name: "测量记录" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(screen.getByTestId("measurement-url")).toHaveTextContent(
      "tab=records",
    );
  });

  it("restores unsaved edits after leaving and returning to the same project page", async () => {
    const calls = mockApi({ existing: true });
    const user = userEvent.setup();
    const page = renderPage(
      "/app/tenant-1/project-1/measurement?tab=sets&set=set-1",
    );
    await user.click(
      await screen.findByRole("button", { name: "基于当前版本修订" }),
    );
    await user.clear(screen.getByLabelText("问题 1"));
    await user.type(screen.getByLabelText("问题 1"), "返回后继续编辑");
    const stored = JSON.parse(
      sessionStorage.getItem(
        "measurement-drafts:user-1:operator-1:tenant-1:project-1",
      )!,
    ) as { drafts: Record<string, { key: string }> };
    const revisionKey = stored.drafts["set-1"].key;
    page.unmount();
    renderPage("/app/tenant-1/project-1/measurement?tab=sets&set=set-1");
    expect(await screen.findByLabelText("问题 1")).toHaveValue(
      "返回后继续编辑",
    );
    await user.click(screen.getByRole("button", { name: "保存为新版本" }));
    await screen.findByRole("heading", { name: "基础问题 · v2" });
    const revision = calls.find(
      ({ path, method }) =>
        method === "POST" && path.endsWith("/question-sets/set-1/versions"),
    );
    expect(revision?.body).toMatchObject({ idempotency_key: revisionKey });
  });

  it("reuses the same create request after an uncertain response and route return", async () => {
    const calls = mockApi({ createErrorOnce: true });
    const user = userEvent.setup();
    const page = renderPage();
    await user.click(screen.getByRole("tab", { name: "问题集" }));
    await user.click(await screen.findByRole("button", { name: "新建问题集" }));
    await user.type(screen.getByLabelText("问题集名称"), "待确认问题");
    await user.type(screen.getByLabelText("每行一个问题"), "如何选择？");
    await user.click(screen.getByRole("button", { name: "创建并保存问题集" }));
    await screen.findByText("问题集创建失败");
    const attempts = () =>
      calls.filter(
        ({ path, method }) =>
          method === "POST" &&
          path.endsWith("/projects/project-1/question-sets"),
      );
    const first = attempts()[0];
    expect(first).toBeDefined();
    page.unmount();
    renderPage("/app/tenant-1/project-1/measurement?tab=sets");
    expect(await screen.findByLabelText("问题集名称")).toHaveValue(
      "待确认问题",
    );
    expect(screen.getByLabelText("每行一个问题")).toHaveValue("如何选择？");
    await user.click(screen.getByRole("button", { name: "创建并保存问题集" }));
    await screen.findByRole("heading", { name: "待确认问题 · v1" });
    expect(attempts()).toHaveLength(2);
    expect(attempts()[1].body).toEqual(first.body);
    expect(attempts()[1].headers.get("Idempotency-Key")).toBe(
      first.headers.get("Idempotency-Key"),
    );
  });

  it("discards malformed stored drafts without breaking measurement or editing", async () => {
    sessionStorage.setItem(
      "measurement-drafts:user-1:operator-1:tenant-1:project-1",
      JSON.stringify({
        name: 12,
        creating: true,
        drafts: { "set-1": { name: "broken", questions: {} } },
      }),
    );
    mockApi({ existing: true });
    const user = userEvent.setup();
    renderPage("/app/tenant-1/project-1/measurement?tab=sets&set=set-1");
    await screen.findByRole("heading", { name: "基础问题 · v1" });
    expect(screen.queryByLabelText("问题集名称")).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "新建问题集" }));
    expect(screen.getByLabelText("问题集名称")).toHaveValue("");
    await user.click(screen.getByRole("button", { name: "基于当前版本修订" }));
    expect(screen.getByLabelText("问题 1")).toHaveValue("怎样选型？");
  });

  it("localizes source and purpose labels without leaking source enums", async () => {
    mockApi({
      existing: true,
      sourceKinds: [
        "user_provided",
        "sales_consultation",
        "product",
        "faq",
        "generated",
        "future_internal",
      ],
    });
    renderPage("/app/tenant-1/project-1/measurement?tab=sets&set=set-1");
    expect(
      await screen.findByText(/用户提供/, { selector: "p" }),
    ).toBeInTheDocument();
    expect(screen.getByText(/销售咨询/, { selector: "p" })).toBeInTheDocument();
    expect(screen.getByText(/产品资料/, { selector: "p" })).toBeInTheDocument();
    expect(screen.getByText(/常见问题/, { selector: "p" })).toBeInTheDocument();
    expect(screen.getByText(/系统生成/, { selector: "p" })).toBeInTheDocument();
    expect(
      screen.getByText(/来源未说明/, { selector: "p" }),
    ).toBeInTheDocument();
    expect(screen.getByText("优化问题")).toBeInTheDocument();
    expect(screen.getAllByText("冻结评估 · 不进入优化")).toHaveLength(5);
    for (const sourceKind of [
      "user_provided",
      "sales_consultation",
      "product",
      "faq",
      "generated",
      "future_internal",
    ]) {
      expect(screen.queryByText(sourceKind)).not.toBeInTheDocument();
    }
    expect(screen.getByText(/创建于/)).toHaveTextContent("2026");

    await i18n.changeLanguage("en");
    expect(
      await screen.findByText(/User-provided/, { selector: "p" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/Sales consultation/, { selector: "p" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/Product information/, { selector: "p" }),
    ).toBeInTheDocument();
    expect(screen.getByText(/FAQ/, { selector: "p" })).toBeInTheDocument();
    expect(
      screen.getByText(/System-generated/, { selector: "p" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/Source not specified/, { selector: "p" }),
    ).toBeInTheDocument();
    expect(screen.getByText("Optimization question")).toBeInTheDocument();
    expect(
      screen.getAllByText("Frozen evaluation · excluded from optimization"),
    ).toHaveLength(5);
    for (const sourceKind of [
      "user_provided",
      "sales_consultation",
      "product",
      "faq",
      "generated",
      "future_internal",
    ]) {
      expect(screen.queryByText(sourceKind)).not.toBeInTheDocument();
    }
    expect(screen.getByText(/Created/)).toHaveTextContent("2026");
  });
});
