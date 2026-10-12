import { afterEach, beforeEach, expect, it, vi } from "vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { OperatorLoginQueue } from "./OperatorLoginQueue";
import * as api from "../api/loginQueues";
import { ApiError } from "../api/client";
import i18n from "../i18n";

const auth = vi.hoisted(() => ({
  user: "user-a",
  operator: "operator-a",
  role: "resource_admin",
  expires: "",
}));
vi.mock("../auth/AuthProvider", () => ({
  useAuth: () => ({
    session: {
      user: { id: auth.user },
      operator: { id: auth.operator },
      memberships: [{ role: auth.role }],
      expires_at: auth.expires,
      csrf_token: auth.expires,
    },
  }),
}));
vi.mock("../api/loginQueues", () => ({
  getActiveLoginQueue: vi.fn(),
  getLoginQueue: vi.fn(),
  createLoginQueue: vi.fn(),
  actOnLoginQueue: vi.fn(),
}));
const queue: api.LoginQueueView = {
  queue_id: "queue-a",
  import_key: "import-a",
  status: "running",
  version: 1,
  items: [
    {
      position: 0,
      row_key: "row-a",
      account_id: "account-a",
      status: "current",
      attempt: 0,
      error_code: null,
    },
    {
      position: 1,
      row_key: "row-b",
      account_id: "account-b",
      status: "pending",
      attempt: 0,
      error_code: null,
    },
  ],
  current: {
    position: 0,
    account_id: "account-a",
    session_id: "session-a",
    phase: "awaiting_login",
    requires_session_recovery: false,
  },
  can_drive: false,
  created_at: "",
  updated_at: "",
};
function show({
  selection,
  directBusy = false,
  onActiveChange,
}: {
  selection?: api.LoginQueueSelection;
  directBusy?: boolean;
  onActiveChange?: (active: boolean) => void;
} = {}) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const tree = () => (
    <FluentProvider theme={webLightTheme}>
      <QueryClientProvider client={client}>
        <OperatorLoginQueue
          selection={selection}
          directBusy={directBusy}
          onActiveChange={onActiveChange}
          renderDesktop={({ sessionId, complete, pause }) => (
            <section>
              <span data-testid="desktop">{sessionId}</span>
              <button onClick={() => void complete().catch(() => {})}>
                Complete desktop
              </button>
              <button onClick={() => void pause().catch(() => {})}>
                Pause desktop
              </button>
            </section>
          )}
        />
      </QueryClientProvider>
    </FluentProvider>
  );
  const view = render(tree());
  return {
    ...view,
    client,
    changeIdentity: () => view.rerender(tree()),
    changeSelection: (next: api.LoginQueueSelection) => {
      selection = next;
      view.rerender(tree());
    },
  };
}
beforeEach(async () => {
  vi.resetAllMocks();
  auth.user = "user-a";
  auth.operator = "operator-a";
  auth.role = "resource_admin";
  auth.expires = "";
  await i18n.changeLanguage("en");
  vi.mocked(api.getActiveLoginQueue).mockResolvedValue(queue);
  vi.mocked(api.getLoginQueue).mockResolvedValue(queue);
  vi.mocked(api.actOnLoginQueue).mockResolvedValue(queue);
});
afterEach(cleanup);
it("does not request queues for unauthorized project users", () => {
  auth.role = "tenant_admin";
  show();
  expect(api.getActiveLoginQueue).not.toHaveBeenCalled();
  expect(
    screen.queryByText("Log in to imported accounts"),
  ).not.toBeInTheDocument();
});
it("restores the same current session after remount without starting another login", async () => {
  const first = show();
  expect(await screen.findByTestId("desktop")).toHaveTextContent("session-a");
  first.unmount();
  show();
  expect(await screen.findByTestId("desktop")).toHaveTextContent("session-a");
  expect(api.actOnLoginQueue).not.toHaveBeenCalled();
  expect(api.createLoginQueue).not.toHaveBeenCalled();
});
it("advances only from the completion endpoint's authoritative next session", async () => {
  const next = {
    ...queue,
    version: 2,
    items: [
      { ...queue.items[0], status: "succeeded" as const },
      { ...queue.items[1], status: "current" as const },
    ],
    current: {
      ...queue.current!,
      position: 1,
      account_id: "account-b",
      session_id: "session-b",
    },
  };
  vi.mocked(api.actOnLoginQueue).mockResolvedValue(next);
  show();
  await screen.findByTestId("desktop");
  fireEvent.click(screen.getByText("Complete desktop"));
  await waitFor(() =>
    expect(screen.getByTestId("desktop")).toHaveTextContent("session-b"),
  );
  expect(screen.getByText(/1 of 2 accounts connected/)).toBeInTheDocument();
  expect(api.actOnLoginQueue).toHaveBeenCalledExactlyOnceWith(
    "queue-a",
    "complete",
    1,
  );
});
it.each([404, 409, 503])(
  "does not advance on completion failure %s",
  async (status) => {
    vi.mocked(api.actOnLoginQueue).mockRejectedValue(
      new ApiError(status, {}, "unsafe upstream message"),
    );
    show();
    await screen.findByTestId("desktop");
    fireEvent.click(screen.getByText("Complete desktop"));
    await screen.findByText(
      status === 409 ? /Login progress changed/ : /Unable to complete/,
    );
    expect(screen.getByTestId("desktop")).toHaveTextContent("session-a");
    expect(screen.getByText(/0 of 2 accounts connected/)).toBeInTheDocument();
    expect(api.actOnLoginQueue).toHaveBeenCalledTimes(1);
    expect(
      screen.queryByText("unsafe upstream message"),
    ).not.toBeInTheDocument();
  },
);
it("preserves closing state after failed disposal and offers scoped retry/skip", async () => {
  const closing: api.LoginQueueView = {
    ...queue,
    status: "paused",
    version: 2,
    current: { ...queue.current!, phase: "closing" },
    can_drive: false,
  };
  vi.mocked(api.actOnLoginQueue).mockRejectedValue(
    new ApiError(503, {}, "unknown close"),
  );
  vi.mocked(api.getLoginQueue)
    .mockResolvedValueOnce(queue)
    .mockResolvedValue(closing);
  show();
  await screen.findByTestId("desktop");
  fireEvent.click(screen.getByText("Complete desktop"));
  await screen.findByText("Waiting for the previous window to close.");
  expect(screen.queryByTestId("desktop")).not.toBeInTheDocument();
  expect(
    screen.getByRole("button", { name: "Retry current account" }),
  ).toBeDisabled();
  fireEvent.click(screen.getByRole("button", { name: "Reload" }));
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: "Retry current account" }),
    ).not.toBeDisabled(),
  );
  expect(api.actOnLoginQueue).toHaveBeenCalledTimes(1);
});
it("requires recovery after reauthentication without exposing the previous desktop", async () => {
  vi.mocked(api.getActiveLoginQueue).mockResolvedValue({
    ...queue,
    current: {
      ...queue.current!,
      phase: "recovery_required",
      requires_session_recovery: true,
      session_id: null,
    },
  });
  vi.mocked(api.getLoginQueue).mockResolvedValue({
    ...queue,
    current: {
      ...queue.current!,
      phase: "recovery_required",
      requires_session_recovery: true,
      session_id: null,
    },
  });
  show();
  await screen.findByText(/Session recovery required/);
  expect(screen.queryByTestId("desktop")).not.toBeInTheDocument();
  expect(api.actOnLoginQueue).not.toHaveBeenCalled();
  fireEvent.click(
    screen.getByRole("button", { name: "Retry current account" }),
  );
  await waitFor(() =>
    expect(api.actOnLoginQueue).toHaveBeenCalledExactlyOnceWith(
      "queue-a",
      "retry",
      1,
    ),
  );
});
it("never drives paused state even if an inconsistent projection sets can_drive", async () => {
  const paused = {
    ...queue,
    status: "paused" as const,
    can_drive: true,
    current: null,
  };
  vi.mocked(api.getActiveLoginQueue).mockResolvedValue(paused);
  vi.mocked(api.getLoginQueue).mockResolvedValue(paused);
  show();
  await screen.findByRole("button", { name: "Resume" });
  expect(api.actOnLoginQueue).not.toHaveBeenCalled();
  expect(screen.queryByTestId("desktop")).not.toBeInTheDocument();
});
it("drives a recoverable running projection once, never fan-outs locally", async () => {
  const ready = { ...queue, current: null, can_drive: true };
  vi.mocked(api.getActiveLoginQueue).mockResolvedValue(ready);
  vi.mocked(api.getLoginQueue).mockResolvedValue(ready);
  show();
  await waitFor(() =>
    expect(api.actOnLoginQueue).toHaveBeenCalledExactlyOnceWith(
      "queue-a",
      "drive",
      1,
    ),
  );
  await screen.findByTestId("desktop");
});
it("does not drive while a direct-login window is in use", async () => {
  vi.mocked(api.getActiveLoginQueue).mockResolvedValue({
    ...queue,
    current: null,
    can_drive: true,
  });
  vi.mocked(api.getLoginQueue).mockResolvedValue({
    ...queue,
    current: null,
    can_drive: true,
  });
  show({ directBusy: true });
  await screen.findByText(/Finish or close/);
  expect(api.actOnLoginQueue).not.toHaveBeenCalled();
});
it("freezes import selection and idempotent queue ID across ambiguous create retry", async () => {
  vi.mocked(api.getActiveLoginQueue).mockResolvedValue(null);
  vi.mocked(api.createLoginQueue)
    .mockRejectedValueOnce(new ApiError(0, {}, "network"))
    .mockResolvedValue(queue);
  const selection = {
    import_key: "import-a",
    items: [
      { row_key: "row-b", account_id: "account-b" },
      { row_key: "row-a", account_id: "account-a" },
    ],
  };
  show({ selection });
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: "Start logins" }),
    ).not.toBeDisabled(),
  );
  fireEvent.click(screen.getByRole("button", { name: "Start logins" }));
  await screen.findByRole("button", {
    name: "Retry starting this login order",
  });
  const initial = vi.mocked(api.createLoginQueue).mock.calls[0][0];
  expect(initial.items).toEqual(selection.items);
  fireEvent.click(
    screen.getByRole("button", { name: "Retry starting this login order" }),
  );
  await screen.findByTestId("desktop");
  expect(api.createLoginQueue).toHaveBeenLastCalledWith(initial);
});
it.each(["user", "operator"] as const)(
  "resets and scopes queries when %s changes",
  async (field) => {
    const view = show();
    await screen.findByTestId("desktop");
    vi.mocked(api.getActiveLoginQueue).mockResolvedValue(null);
    auth[field] = `${field}-b`;
    view.changeIdentity();
    await screen.findByText(
      "Select imported accounts below to prepare a login order.",
    );
    expect(screen.queryByTestId("desktop")).not.toBeInTheDocument();
    expect(
      view.client
        .getQueryCache()
        .getAll()
        .map((entry) => entry.queryKey.slice(0, 3)),
    ).toContainEqual(["operator-login-queue", auth.user, auth.operator]);
  },
);

it("reconnects through a fresh projection after same-user reauthentication", async () => {
  auth.expires = "2026-01-01T00:00:00Z";
  const view = show();
  await screen.findByTestId("desktop");
  const recovered: api.LoginQueueView = {
    ...queue,
    current: {
      ...queue.current!,
      session_id: null,
      phase: "recovery_required",
      requires_session_recovery: true,
    },
  };
  vi.mocked(api.getActiveLoginQueue).mockResolvedValue(recovered);
  vi.mocked(api.getLoginQueue).mockResolvedValue(recovered);
  auth.expires = "2026-01-02T00:00:00Z";
  view.changeIdentity();
  expect(screen.queryByTestId("desktop")).not.toBeInTheDocument();
  await screen.findByText(/Session recovery required/);
  expect(api.actOnLoginQueue).not.toHaveBeenCalled();
});

it("uses explicit recovery for an unknown closing operation without claiming verification", async () => {
  const closing: api.LoginQueueView = {
    ...queue,
    status: "paused",
    current: { ...queue.current!, phase: "closing" },
  };
  vi.mocked(api.getActiveLoginQueue).mockResolvedValue(closing);
  vi.mocked(api.getLoginQueue).mockResolvedValue(closing);
  show();
  fireEvent.click(
    await screen.findByRole("button", { name: "Retry current account" }),
  );
  await waitFor(() =>
    expect(api.actOnLoginQueue).toHaveBeenCalledExactlyOnceWith(
      "queue-a",
      "retry",
      1,
    ),
  );
  expect(api.createLoginQueue).not.toHaveBeenCalled();
});

it("serializes concurrent completion clicks without optimistic advancement", async () => {
  let resolve!: (value: api.LoginQueueView) => void;
  vi.mocked(api.actOnLoginQueue).mockImplementation(
    () =>
      new Promise((done) => {
        resolve = done;
      }),
  );
  show();
  await screen.findByTestId("desktop");
  fireEvent.click(screen.getByText("Complete desktop"));
  fireEvent.click(screen.getByText("Complete desktop"));
  await waitFor(() => expect(api.actOnLoginQueue).toHaveBeenCalledTimes(1));
  expect(screen.getByTestId("desktop")).toHaveTextContent("session-a");
  await act(async () => resolve(queue));
});

it("reserves direct login during initial recovery and failed lookup until recovery resolves", async () => {
  let reject!: (error: Error) => void;
  vi.mocked(api.getActiveLoginQueue).mockImplementationOnce(
    () =>
      new Promise((_, fail) => {
        reject = fail;
      }),
  );
  const onActiveChange = vi.fn();
  show({ onActiveChange });
  expect(onActiveChange).toHaveBeenLastCalledWith(true);
  await act(async () => reject(new Error("offline")));
  await screen.findByText(/Unable to complete/);
  expect(onActiveChange).toHaveBeenLastCalledWith(true);
  vi.mocked(api.getActiveLoginQueue).mockResolvedValue(null);
  fireEvent.click(screen.getByRole("button", { name: "Reload" }));
  await waitFor(() => expect(onActiveChange).toHaveBeenLastCalledWith(false));
});

it("reserves direct login throughout deferred creation and ambiguous readback", async () => {
  vi.mocked(api.getActiveLoginQueue).mockResolvedValue(null);
  let reject!: (error: Error) => void;
  vi.mocked(api.createLoginQueue).mockImplementationOnce(
    () =>
      new Promise((_, fail) => {
        reject = fail;
      }),
  );
  const onActiveChange = vi.fn();
  show({
    onActiveChange,
    selection: {
      import_key: "import-a",
      items: [{ row_key: "row-a", account_id: "account-a" }],
    },
  });
  await waitFor(() => expect(onActiveChange).toHaveBeenLastCalledWith(false));
  fireEvent.click(screen.getByRole("button", { name: "Start logins" }));
  expect(onActiveChange).toHaveBeenLastCalledWith(true);
  await act(async () => reject(new Error("offline")));
  await screen.findByRole("button", {
    name: "Retry starting this login order",
  });
  expect(onActiveChange).toHaveBeenLastCalledWith(true);
});

it("clears a reconciled create so a different selection starts a fresh queue after completion", async () => {
  vi.mocked(api.getActiveLoginQueue).mockResolvedValueOnce(null);
  vi.mocked(api.createLoginQueue).mockImplementationOnce(async (request) => {
    const recovered = { ...queue, queue_id: request.queue_id };
    vi.mocked(api.getActiveLoginQueue).mockResolvedValue(recovered);
    vi.mocked(api.getLoginQueue).mockResolvedValue(recovered);
    vi.mocked(api.actOnLoginQueue).mockResolvedValue({
      ...recovered,
      version: 2,
      status: "completed",
      current: null,
    });
    throw new ApiError(0, {}, "network");
  });
  const firstSelection = {
    import_key: "import-a",
    items: [{ row_key: "row-a", account_id: "account-a" }],
  };
  const view = show({ selection: firstSelection });
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: "Start logins" }),
    ).not.toBeDisabled(),
  );
  fireEvent.click(screen.getByRole("button", { name: "Start logins" }));
  await screen.findByTestId("desktop");
  const firstRequest = vi.mocked(api.createLoginQueue).mock.calls[0][0];
  fireEvent.click(screen.getByText("Complete desktop"));
  await screen.findByText(/Login order completed/);
  const secondSelection = {
    import_key: "import-b",
    items: [{ row_key: "row-b", account_id: "account-b" }],
  };
  view.changeSelection(secondSelection);
  vi.mocked(api.createLoginQueue).mockResolvedValue(queue);
  fireEvent.click(screen.getByRole("button", { name: "Start logins" }));
  await waitFor(() => expect(api.createLoginQueue).toHaveBeenCalledTimes(2));
  const secondRequest = vi.mocked(api.createLoginQueue).mock.calls[1][0];
  expect(secondRequest.queue_id).not.toBe(firstRequest.queue_id);
  expect(secondRequest).toMatchObject(secondSelection);
});
