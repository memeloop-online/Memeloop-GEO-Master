import { beforeEach, expect, it, vi } from "vitest";
import { apiFetch } from "./client";
import {
  actOnLoginQueue,
  createLoginQueue,
  getActiveLoginQueue,
  getLoginQueue,
} from "./loginQueues";
vi.mock("./client", () => ({ apiFetch: vi.fn() }));
beforeEach(() => vi.clearAllMocks());
it("reads active and exact queue snapshots without client identity selectors", async () => {
  await getActiveLoginQueue();
  await getLoginQueue("queue/a");
  expect(apiFetch).toHaveBeenCalledWith(
    "/operator/channel-login-queues/active",
  );
  expect(apiFetch).toHaveBeenCalledWith(
    "/operator/channel-login-queues/queue%2Fa",
  );
});
it("creates the ordered snapshot using its durable idempotency identity", async () => {
  const body = {
    queue_id: "queue-a",
    import_key: "import-a",
    items: [
      { row_key: "row-b", account_id: "account-b" },
      { row_key: "row-a", account_id: "account-a" },
    ],
  };
  await createLoginQueue(body);
  expect(apiFetch).toHaveBeenCalledExactlyOnceWith(
    "/operator/channel-login-queues",
    { method: "POST", body, idempotency: "omit" },
  );
});
it.each(["drive", "pause", "resume", "retry", "skip", "complete"] as const)(
  "uses CAS for %s without a direct-login mutation",
  async (action) => {
    await actOnLoginQueue("queue-a", action, 7);
    expect(apiFetch).toHaveBeenCalledExactlyOnceWith(
      `/operator/channel-login-queues/queue-a/${action}`,
      { method: "POST", body: { expected_version: 7 }, idempotency: "omit" },
    );
  },
);
