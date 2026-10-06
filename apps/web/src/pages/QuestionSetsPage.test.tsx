import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { AuthProvider } from "../auth/AuthProvider";
import { setCsrfToken, setUnauthorizedHandler } from "../api/client";
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
}: {
  existing?: boolean;
  role?: string;
  conflict?: boolean;
  listError?: boolean;
} = {}) {
  const calls: {
    path: string;
    method: string;
    body: unknown;
    headers: Headers;
  }[] = [];
  let exists = existing;
  let current = { ...version, parent_version_id: null as string | null };
  vi.stubGlobal(
    "fetch",
    vi.fn((request: RequestInfo | URL, init?: RequestInit) => {
      const path = new URL(String(request), "http://localhost").pathname;
      const method = init?.method ?? "GET";
      const body = init?.body ? JSON.parse(String(init.body)) : null;
      calls.push({ path, method, body, headers: new Headers(init?.headers) });
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

function renderPage() {
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
          <MemoryRouter
            initialEntries={["/app/tenant-1/project-1/measurement"]}
          >
            <Routes>
              <Route
                path="/app/:tenantId/:projectId/measurement"
                element={<QuestionSetsPage />}
              />
            </Routes>
          </MemoryRouter>
        </AuthProvider>
      </FluentProvider>
    </QueryClientProvider>,
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
  setCsrfToken(undefined);
  setUnauthorizedHandler(undefined);
});

describe("P13 versioned question sets", () => {
  it("creates a named immutable version from pasted lines and edits into a second version", async () => {
    const calls = mockApi();
    const user = userEvent.setup();
    renderPage();
    expect(await screen.findByText("尚无问题集")).toBeInTheDocument();
    await user.type(screen.getByLabelText("问题集名称"), "基础问题");
    await user.type(
      screen.getByLabelText("每行一个问题"),
      "怎样选型？{enter}如何维护？",
    );
    await user.click(screen.getByRole("button", { name: "创建并封存 v1" }));
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

  it("supports a read-only project and recoverable list failure", async () => {
    mockApi({ existing: true, role: "viewer" });
    renderPage();
    expect(await screen.findByText(/当前角色为只读/)).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "创建并封存 v1" }),
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
    expect(await screen.findByText("问题集无法读取")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /重试/ })).toBeInTheDocument();
    expect(
      calls.filter(
        (item) => item.path.endsWith("/question-sets") && item.method === "GET",
      ),
    ).toHaveLength(1);
  });
});
