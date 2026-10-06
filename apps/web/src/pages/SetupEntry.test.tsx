import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { SetupEntry } from "./SetupEntry";
import { createProject } from "../api/projects";
import { createAgentConversation } from "../api/agent";

const session = {
  user: { id: "entry-user" },
  operator: { id: "entry-operator" },
  memberships: [{ tenant_id: "entry-tenant", role: "tenant_admin" }],
};
vi.mock("../auth/AuthProvider", () => ({
  useAuth: () => ({ session }),
}));
vi.mock("../api/projects", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../api/projects")>()),
  createProject: vi.fn(),
}));
vi.mock("../api/agent", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../api/agent")>()),
  createAgentConversation: vi.fn(),
}));

function Destination() {
  return <p>{useLocation().pathname}</p>;
}
function renderEntry(tenant = "entry-tenant") {
  return render(
    <StrictMode>
      <FluentProvider theme={webLightTheme}>
        <QueryClientProvider client={new QueryClient()}>
          <MemoryRouter initialEntries={[`/setup?tenant_id=${tenant}`]}>
            <Routes>
              <Route path="/setup" element={<SetupEntry />} />
              <Route path="*" element={<Destination />} />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>
      </FluentProvider>
    </StrictMode>,
  );
}
beforeEach(() => {
  sessionStorage.clear();
  vi.mocked(createProject).mockResolvedValue({ id: "draft-project" } as Awaited<
    ReturnType<typeof createProject>
  >);
  vi.mocked(createAgentConversation).mockResolvedValue({
    id: "first-conversation",
  } as Awaited<ReturnType<typeof createAgentConversation>>);
});
afterEach(() => vi.resetAllMocks());

describe("chat-first project entry", () => {
  it("creates only an empty draft without forms, conversations or a turn", async () => {
    renderEntry();
    expect(
      await screen.findByText("/app/entry-tenant/draft-project/chat"),
    ).toBeInTheDocument();
    expect(createProject).toHaveBeenCalledExactlyOnceWith(
      "entry-tenant",
      { display_name: "新项目", settings: {} },
      expect.any(String),
    );
    expect(createAgentConversation).not.toHaveBeenCalled();
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
    expect(sessionStorage.length).toBe(0);
  });

  it("reuses the draft creation key after failure and refresh", async () => {
    vi.mocked(createProject).mockRejectedValueOnce(
      new Error("connection lost"),
    );
    const first = renderEntry();
    await screen.findByText("暂时无法打开项目对话");
    const key = vi.mocked(createProject).mock.calls[0][2];
    first.unmount();
    renderEntry();
    await screen.findByText("/app/entry-tenant/draft-project/chat");
    expect(createProject).toHaveBeenCalledTimes(2);
    expect(createProject).toHaveBeenLastCalledWith(
      "entry-tenant",
      { display_name: "新项目", settings: {} },
      key,
    );
  });

  it("retains the project key for an uncertain creation result and guards rapid retries", async () => {
    vi.mocked(createProject).mockRejectedValueOnce(
      new Error("connection lost"),
    );
    renderEntry();
    await screen.findByText("暂时无法打开项目对话");
    const key = vi.mocked(createProject).mock.calls[0][2];
    const retry = screen.getByRole("button", { name: "重试" });
    fireEvent.click(retry);
    fireEvent.click(retry);
    await screen.findByText("/app/entry-tenant/draft-project/chat");
    expect(createProject).toHaveBeenCalledTimes(2);
    expect(createProject).toHaveBeenLastCalledWith(
      "entry-tenant",
      { display_name: "新项目", settings: {} },
      key,
    );
    expect(createAgentConversation).not.toHaveBeenCalled();
  });

  it("does not create anything in an unauthorized tenant", async () => {
    renderEntry("other-tenant");
    await waitFor(() =>
      expect(screen.getByText("权限不足")).toBeInTheDocument(),
    );
    expect(createProject).not.toHaveBeenCalled();
    expect(createAgentConversation).not.toHaveBeenCalled();
  });
});
