import { afterEach, describe, expect, it, vi } from "vitest";
import {
  agentEventStreamUrl,
  cancelAgentTurn,
  listAgentConversations,
  postAgentMessage,
  uploadAgentAttachment,
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

  it("uploads real bytes, hashes them, then returns a durable reference without importing knowledge", async () => {
    const bytes = new Uint8Array([1, 2, 3, 4]);
    const file = new File([bytes], "notes.txt", { type: "text/plain" });
    Object.defineProperty(file, "arrayBuffer", {
      value: () => Promise.resolve(bytes.buffer),
    });
    const digest = vi.fn().mockResolvedValue(new Uint8Array(32).fill(0xab));
    vi.stubGlobal("crypto", {
      subtle: { digest },
      randomUUID: () => "uuid-a",
    });
    const reference = {
      attachment_id: "attachment-a",
      object_id: "object-a",
      filename: "notes.txt",
      media_type: "text/plain",
      size_bytes: 4,
      sha256: "ab".repeat(32),
      object_version: "1",
    };
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(response({ upload_session_id: "session-a" }, 201))
      .mockResolvedValueOnce(response({ upload_session_id: "session-a" }))
      .mockResolvedValueOnce(response(reference));
    vi.stubGlobal("fetch", fetchMock);
    setCsrfToken("csrf-a");

    const progress: string[] = [];
    const uploaded = await uploadAgentAttachment(
      "tenant-a",
      "project-a",
      file,
      {
        createKey: "create-a",
        completeKey: "complete-a",
        onProgress: (state) => progress.push(state),
      },
    );

    expect(uploaded).toEqual(reference);
    expect(digest).toHaveBeenCalledWith("SHA-256", bytes.buffer);
    expect(progress).toEqual([
      "creating_session",
      "uploading",
      "completing",
      "uploaded",
    ]);
    expect(fetchMock).toHaveBeenCalledTimes(3);
    const [createUrl, createInit] = fetchMock.mock.calls[0] as [
      string,
      RequestInit,
    ];
    expect(createUrl).toBe(
      "/api/v1/agent/attachments/upload-sessions?tenant_id=tenant-a&project_id=project-a",
    );
    expect(new Headers(createInit.headers).get("Idempotency-Key")).toBe(
      "create-a",
    );
    expect(JSON.parse(String(createInit.body))).toEqual({
      filename: "notes.txt",
      declared_media_type: "text/plain",
      expected_size: 4,
      expected_sha256: "ab".repeat(32),
    });
    const [putUrl, putInit] = fetchMock.mock.calls[1] as [string, RequestInit];
    expect(putUrl).toContain(
      "/agent/attachments/upload-sessions/session-a/content?",
    );
    expect(putInit.body).toBe(file);
    expect(new Headers(putInit.headers).get("Content-Type")).toBe(
      "application/octet-stream",
    );
    expect(new Headers(putInit.headers).get("X-CSRF-Token")).toBe("csrf-a");
    const [completeUrl, completeInit] = fetchMock.mock.calls[2] as [
      string,
      RequestInit,
    ];
    expect(completeUrl).toContain(
      "/agent/attachments/upload-sessions/session-a/complete?",
    );
    expect(new Headers(completeInit.headers).get("Idempotency-Key")).toBe(
      "complete-a",
    );
    expect(JSON.parse(String(completeInit.body))).toEqual({});
    expect(
      fetchMock.mock.calls.some(([url]) => String(url).includes("/knowledge/")),
    ).toBe(false);
  });

  it("retries the staged upload with its original session and command key", async () => {
    const file = new File(["x"], "retry.txt", { type: "text/plain" });
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(response({ upload_session_id: "session-a" }))
      .mockResolvedValueOnce(
        response({
          attachment_id: "attachment-a",
          object_id: "object-a",
          filename: "retry.txt",
        }),
      );
    vi.stubGlobal("fetch", fetchMock);

    await uploadAgentAttachment("tenant-a", "project-a", file, {
      sessionId: "session-a",
      createKey: "create-a",
      completeKey: "complete-a",
    });

    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(String(fetchMock.mock.calls[0][0])).toContain("/session-a/content?");
    expect(
      new Headers(fetchMock.mock.calls[1][1].headers).get("Idempotency-Key"),
    ).toBe("complete-a");
  });

  it("retries an uncertain completion with the same key without resending bytes", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      response({
        attachment_id: "attachment-a",
        object_id: "object-a",
        filename: "retry.txt",
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    await uploadAgentAttachment(
      "tenant-a",
      "project-a",
      new File(["x"], "retry.txt"),
      {
        sessionId: "session-a",
        contentUploaded: true,
        createKey: "create-a",
        completeKey: "complete-a",
      },
    );

    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(String(fetchMock.mock.calls[0][0])).toContain(
      "/session-a/complete?",
    );
    expect(
      new Headers(fetchMock.mock.calls[0][1].headers).get("Idempotency-Key"),
    ).toBe("complete-a");
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
