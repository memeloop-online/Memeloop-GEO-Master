import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { ConversationMessageListProjection } from "memeloop";
import { AgentMessageContent } from "./AgentMessageContent";

afterEach(cleanup);

function message(content: string): ConversationMessageListProjection {
  return {
    messageId: "answer-a",
    turnId: "turn-a",
    conversationId: "conversation-a",
    originNodeId: "geo-api",
    originSequence: 1,
    timestamp: 0,
    lamportClock: 1,
    role: "assistant",
    content,
  };
}

describe("assistant Markdown content", () => {
  it("retains the native notice when the assistant text itself is shortened", () => {
    render(
      <AgentMessageContent
        message={{
          ...message("## Partial answer"),
          metadata: {
            displayTruncation: {
              truncated: true,
              originalCharacterCount: 50000,
              originalEstimatedBytes: 50000,
              originalEstimatedRenderRows: 1000,
              contentTruncated: true,
              omittedFields: [],
              capability: "export",
            },
          },
        }}
      />,
    );
    expect(screen.getByTestId("message-display-truncated")).toBeInTheDocument();
    expect(screen.getByText("## Partial answer")).toBeInTheDocument();
  });

  it("keeps unsafe links, HTML, and remote images inert", () => {
    const { container } = render(
      <AgentMessageContent
        message={message(
          '[unsafe](javascript:alert%281%29)\n\n[relative](/app/tenant-a/project-a/measurement)\n\n![descriptive image](https://example.org/image.png)\n\n<script>alert(1)</script>\n\n<a href="https://example.org/raw">raw</a>\n\n[Source](https://example.org/source)',
        )}
      />,
    );

    expect(screen.getByText("unsafe")).not.toHaveAttribute("href");
    expect(screen.getByRole("link", { name: "relative" })).toHaveAttribute(
      "href",
      "/app/tenant-a/project-a/measurement",
    );
    expect(screen.getByText("descriptive image")).toBeInTheDocument();
    expect(container.querySelector("script, img")).toBeNull();
    expect(screen.getAllByRole("link")).toHaveLength(2);
    expect(screen.getByRole("link", { name: "Source" })).toHaveAttribute(
      "rel",
      "noopener noreferrer",
    );
  });

  it("preserves native user text and code literals", () => {
    const { rerender } = render(
      <AgentMessageContent
        message={{
          ...message("## Draft [source](https://example.org)"),
          role: "user",
        }}
      />,
    );
    expect(
      screen.getByText("## Draft [source](https://example.org)"),
    ).toBeInTheDocument();
    expect(screen.queryByRole("heading")).toBeNull();
    expect(screen.queryByRole("link")).toBeNull();

    rerender(
      <AgentMessageContent
        message={message("```text\n## Draft\n| --- |\n``` ")}
      />,
    );
    expect(screen.getByText(/## Draft/).tagName).toBe("CODE");
    expect(screen.queryByRole("heading")).toBeNull();
    expect(screen.queryByRole("table")).toBeNull();
  });
});
