import { afterEach, describe, expect, it, vi } from "vitest";
import {
  agentEventStreamUrl,
  cancelAgentTurn,
  listAgentConversations,
  postAgentMessage,
} from "./agent";
import { setCsrfToken } from "./client";

function response(body: unknown, status = 200) {
  return new Response(body === undefined ? null : JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

afterEach(() => {
  setCsrfToken(undefined);
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("Agent API adapter", () => {
  it("uses scoped selectors to list conversations", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      response({
        items: [],
        next_cursor: null,
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    await listAgentConversations("tenant-a", "project-a");

    expect(String(fetchMock.mock.calls[0][0])).toBe(
      "/api/v1/agent/conversations?tenant_id=tenant-a&project_id=project-a",
    );
  });

  it("posts only durable object attachment references", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      response({
        status: "accepted",
        conversation_id: "conversation-a",
        message_id: "message-a",
        turn_id: "turn-a",
        run_id: "run-a",
        events_url: "/api/v1/agent/conversations/conversation-a/events",
        run_status: "queued",
      }),
    );
    vi.stubGlobal("fetch", fetchMock);
    setCsrfToken("csrf-a");

    await postAgentMessage(
      "tenant-a",
      "project-a",
      "conversation-a",
      {
        content: "请分析知识库覆盖情况",
        attachments: [
          {
            attachment_id: "attachment-a",
            object_id: "object-a",
            filename: "coverage.pdf",
            media_type: "application/pdf",
            size_bytes: 42,
            sha256: "abc",
          },
        ],
      },
      "message-command-a",
    );

    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    const headers = new Headers(init.headers);
    expect(url).toBe(
      "/api/v1/agent/conversations/conversation-a/messages?tenant_id=tenant-a&project_id=project-a",
    );
    expect(init.method).toBe("POST");
    expect(headers.get("Idempotency-Key")).toBe("message-command-a");
    expect(headers.get("X-CSRF-Token")).toBe("csrf-a");
    expect(JSON.parse(String(init.body))).toEqual({
      content: "请分析知识库覆盖情况",
      attachments: [
        {
          attachment_id: "attachment-a",
          object_id: "object-a",
          filename: "coverage.pdf",
          media_type: "application/pdf",
          size_bytes: 42,
          sha256: "abc",
        },
      ],
    });
  });

  it("uses the cancel route and creates a scoped SSE URL", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      response({
        id: "run-a",
        conversation_id: "conversation-a",
        turn_id: "turn-a",
        status: "cancelled",
        capability: { status: "available", runtime: "deno_core" },
        cancel_version: 1,
        created_at: "2026-09-19T00:00:00Z",
        updated_at: "2026-09-19T00:00:00Z",
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    await cancelAgentTurn("tenant-a", "project-a", "turn-a", "cancel-a");

    expect(String(fetchMock.mock.calls[0][0])).toBe(
      "/api/v1/agent/turns/turn-a/cancel?tenant_id=tenant-a&project_id=project-a",
    );
    expect(
      agentEventStreamUrl("tenant-a", "project-a", "conversation-a", "42"),
    ).toBe(
      "/api/v1/agent/conversations/conversation-a/events?tenant_id=tenant-a&project_id=project-a&after=42",
    );
  });
});
