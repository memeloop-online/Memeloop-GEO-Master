import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { TopicQuestionGenerator } from "./TopicQuestionGenerator";
import { createAgentConversation, postAgentMessage } from "../api/agent";

vi.mock("../api/agent", () => ({
  createAgentConversation: vi.fn(),
  postAgentMessage: vi.fn(),
}));
vi.mock("../api/projects", () => ({
  useProjectQuery: () => ({
    data: { settings: { market: "GB", language: "en" } },
  }),
}));

function Destination() {
  return <output data-testid="destination">{useLocation().pathname}</output>;
}

function renderGenerator(canWrite = true) {
  return render(
    <FluentProvider theme={webLightTheme}>
      <MemoryRouter initialEntries={["/app/tenant-1/project-1/measurement"]}>
        <Routes>
          <Route
            path="/app/:tenantId/:projectId/measurement"
            element={
              <TopicQuestionGenerator
                tenantId="tenant-1"
                projectId="project-1"
                canWrite={canWrite}
              />
            }
          />
          <Route
            path="/app/:tenantId/:projectId/chat/:conversationId"
            element={<Destination />}
          />
        </Routes>
      </MemoryRouter>
    </FluentProvider>,
  );
}

beforeEach(() => {
  vi.mocked(createAgentConversation).mockResolvedValue({
    id: "conversation-1",
  } as Awaited<ReturnType<typeof createAgentConversation>>);
  vi.mocked(postAgentMessage).mockResolvedValue({
    status: "accepted",
    conversation_id: "conversation-1",
  } as Awaited<ReturnType<typeof postAgentMessage>>);
});

afterEach(() => vi.resetAllMocks());

describe("topic candidate-question entry", () => {
  it("keeps the topic as user content, uses project defaults and opens only the accepted conversation", async () => {
    const user = userEvent.setup();
    renderGenerator();
    expect(createAgentConversation).not.toHaveBeenCalled();
    await user.type(screen.getByRole("textbox", { name: "话题" }), "家庭储能");
    await user.click(screen.getByRole("button", { name: "生成候选问题" }));
    expect(await screen.findByTestId("destination")).toHaveTextContent(
      "/app/tenant-1/project-1/chat/conversation-1",
    );
    expect(createAgentConversation).toHaveBeenCalledWith(
      "tenant-1",
      "project-1",
      {},
      expect.any(String),
    );
    expect(postAgentMessage).toHaveBeenCalledWith(
      "tenant-1",
      "project-1",
      "conversation-1",
      {
        content: expect.stringContaining(
          '话题："家庭储能"\n市场："GB"\n问题语言："en"',
        ),
      },
      expect.any(String),
    );
  });

  it("retries an uncertain message with the same conversation and message keys", async () => {
    vi.mocked(postAgentMessage).mockRejectedValueOnce(new Error("offline"));
    const user = userEvent.setup();
    renderGenerator();
    await user.type(screen.getByRole("textbox", { name: "话题" }), "储能");
    await user.click(screen.getByRole("button", { name: "生成候选问题" }));
    expect(
      await screen.findByRole("button", { name: "重试提交" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "话题" })).toHaveValue("储能");
    expect(screen.getByRole("textbox", { name: "话题" })).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "重试提交" }));
    expect(await screen.findByTestId("destination")).toHaveTextContent(
      "/chat/conversation-1",
    );
    expect(createAgentConversation).toHaveBeenCalledTimes(1);
    expect(postAgentMessage).toHaveBeenCalledTimes(2);
    expect(vi.mocked(postAgentMessage).mock.calls[1]).toEqual(
      vi.mocked(postAgentMessage).mock.calls[0],
    );
  });

  it("reuses creation key after an uncertain conversation response without claiming generation", async () => {
    vi.mocked(createAgentConversation).mockRejectedValueOnce(
      new Error("offline"),
    );
    const user = userEvent.setup();
    renderGenerator();
    await user.type(screen.getByRole("textbox", { name: "话题" }), "电池");
    await user.click(screen.getByRole("button", { name: "生成候选问题" }));
    await screen.findByRole("button", { name: "重试提交" });
    expect(screen.queryByText(/已生成|已保存/)).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "重试提交" }));
    await waitFor(() =>
      expect(createAgentConversation).toHaveBeenCalledTimes(2),
    );
    expect(vi.mocked(createAgentConversation).mock.calls[1]).toEqual(
      vi.mocked(createAgentConversation).mock.calls[0],
    );
  });

  it("does not allow a read-only member to create a conversation", () => {
    renderGenerator(false);
    expect(screen.getByText(/只能查看问题/)).toBeInTheDocument();
    expect(screen.queryByRole("textbox", { name: "话题" })).toBeNull();
    expect(createAgentConversation).not.toHaveBeenCalled();
  });
});
