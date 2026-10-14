import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  act,
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { MemoryRouter } from "react-router-dom";
import * as api from "../api/partnerConnections";
import { advanceAuthGeneration } from "../api/client";
import i18n from "../i18n";
import { partnerConnectionsResources } from "../i18n/partnerConnections";
import { PartnerConnectionsPanel } from "./PartnerConnectionsPanel";

const auth = vi.hoisted(() => ({
  userId: "synthetic-user",
  role: "tenant_admin",
  status: "authenticated",
}));
vi.mock("../auth/AuthProvider", () => ({
  useAuth: () => ({
    status: auth.status,
    session: {
      user: { id: auth.userId },
      operator: { id: "synthetic-operator" },
      memberships: [{ tenant_id: "tenant", role: auth.role }],
    },
  }),
}));
const connection: api.PartnerConnection = {
  connection_id: "synthetic-connection",
  created_at: "2026-01-01T00:00:00Z",
  revoked_at: null,
  knowledge_write: true,
};
const key: api.PartnerKey = {
  key_id: "synthetic-key",
  created_at: "2026-01-01T00:00:00Z",
  revoked_at: null,
  expires_at: null,
};
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
function setup(keys: api.PartnerKey[] = []) {
  const connections = vi
    .spyOn(api, "listPartnerConnections")
    .mockResolvedValue({ items: [connection], next_cursor: null });
  const list = vi
    .spyOn(api, "listPartnerKeys")
    .mockResolvedValue({ items: keys, next_cursor: null });
  const issue = vi
    .spyOn(api, "issuePartnerKey")
    .mockResolvedValue({ key, bearer: "synthetic-one-time-value" });
  const rotate = vi
    .spyOn(api, "rotatePartnerKey")
    .mockResolvedValue({ key, bearer: "synthetic-rotated-value" });
  const revoke = vi
    .spyOn(api, "revokePartnerKey")
    .mockResolvedValue({ ...key, revoked_at: "2026-01-02T00:00:00Z" });
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const content = (projectId: string) => (
    <QueryClientProvider client={client}>
      <FluentProvider theme={webLightTheme}>
        <MemoryRouter>
          <PartnerConnectionsPanel tenantId="tenant" projectId={projectId} />
        </MemoryRouter>
      </FluentProvider>
    </QueryClientProvider>
  );
  const view = render(content("project"));
  return {
    connections,
    list,
    issue,
    rotate,
    revoke,
    client,
    rerender: (project = "project") => view.rerender(content(project)),
  };
}
beforeEach(async () => {
  auth.userId = "synthetic-user";
  auth.role = "tenant_admin";
  auth.status = "authenticated";
  for (const [locale, resources] of Object.entries(
    partnerConnectionsResources,
  )) {
    i18n.addResourceBundle(locale, "partnerConnections", resources, true, true);
  }
  await i18n.changeLanguage("en");
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("partner connection administration", () => {
  it("allows normal creation and rotation without loading all historic keys", async () => {
    const { list, issue } = setup([key]);
    list.mockResolvedValue({
      items: [key],
      next_cursor: "synthetic-next-page",
    });
    await screen.findByRole("button", { name: "Load more" });
    expect(screen.getByRole("button", { name: "Create key" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Rotate key" })).toBeEnabled();
    await userEvent.click(screen.getByRole("button", { name: "Create key" }));
    expect(
      await screen.findByDisplayValue("synthetic-one-time-value"),
    ).toBeInTheDocument();
    expect(issue).toHaveBeenCalledTimes(1);
    expect(list.mock.calls.every((call) => call[3] === undefined)).toBe(true);
  });
  it("shows metadata, reveals only the issued key, copies and dismisses without caching it", async () => {
    const user = userEvent.setup();
    const { issue, client } = setup();
    const copy = vi.spyOn(navigator.clipboard, "writeText").mockResolvedValue();
    await user.click(await screen.findByRole("button", { name: "Create key" }));
    const region = await screen.findByRole("region", { name: "New API key" });
    expect(within(region).getByRole("textbox")).toHaveValue(
      "synthetic-one-time-value",
    );
    expect(issue).toHaveBeenCalledWith(
      "tenant",
      "project",
      connection.connection_id,
      null,
    );
    await user.click(within(region).getByRole("button", { name: "Copy key" }));
    expect(copy).toHaveBeenCalledWith("synthetic-one-time-value");
    expect(client.getMutationCache().getAll()).toHaveLength(0);
    expect(
      JSON.stringify(
        client
          .getQueryCache()
          .getAll()
          .map((query) => query.state.data),
      ),
    ).not.toContain("synthetic-one-time-value");
    await user.click(
      within(region).getByRole("button", { name: "Dismiss key" }),
    );
    expect(
      screen.queryByRole("region", { name: "New API key" }),
    ).not.toBeInTheDocument();
  });
  it("distinguishes expiry and revocation and never displays a saved bearer", async () => {
    setup([
      {
        ...key,
        key_id: "synthetic-expired",
        expires_at: "2000-01-01T00:00:00Z",
      },
      {
        ...key,
        key_id: "synthetic-revoked",
        revoked_at: "2026-01-02T00:00:00Z",
      },
    ]);
    expect(
      await screen.findByText(/synthetic-expired · Expired/),
    ).toBeInTheDocument();
    expect(screen.getByText(/synthetic-revoked · Revoked/)).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "Revoke key" })).toHaveLength(
      1,
    );
    expect(
      screen.queryByRole("region", { name: "New API key" }),
    ).not.toBeInTheDocument();
  });
  it("rotates explicitly and reveals the replacement without assuming the old key stays valid", async () => {
    const { rotate, list } = setup([key]);
    await screen.findByRole("button", { name: "Rotate key" });
    list.mockResolvedValue({
      items: [
        { ...key, revoked_at: "2026-01-02T00:00:00Z" },
        { ...key, key_id: "synthetic-new-key" },
      ],
      next_cursor: null,
    });
    await userEvent.click(screen.getByRole("button", { name: "Rotate key" }));
    expect(
      await screen.findByDisplayValue("synthetic-rotated-value"),
    ).toBeInTheDocument();
    expect(rotate).toHaveBeenCalledWith(
      "tenant",
      "project",
      connection.connection_id,
      key.key_id,
      null,
    );
    expect(
      await screen.findByText(/synthetic-key · Revoked/),
    ).toBeInTheDocument();
  });
  it("discards a pending completion after unmount", async () => {
    const { issue } = setup();
    const pending = deferred<api.IssuedPartnerKey>();
    issue.mockReturnValue(pending.promise);
    await userEvent.click(
      await screen.findByRole("button", { name: "Create key" }),
    );
    cleanup();
    await act(async () => {
      pending.resolve({ key, bearer: "synthetic-unmounted-value" });
      await pending.promise;
    });
    expect(
      screen.queryByDisplayValue("synthetic-unmounted-value"),
    ).not.toBeInTheDocument();
  });
  it.each(["project", "auth"])(
    "discards a stale issue completion after %s changes",
    async (change) => {
      const user = userEvent.setup();
      const { issue, rerender } = setup();
      const pending = deferred<api.IssuedPartnerKey>();
      issue.mockReturnValue(pending.promise);
      await user.click(
        await screen.findByRole("button", { name: "Create key" }),
      );
      if (change === "auth") {
        advanceAuthGeneration();
        auth.userId = "synthetic-other";
      }
      rerender(change === "project" ? "other-project" : "project");
      await act(async () => {
        pending.resolve({ key, bearer: "synthetic-stale-value" });
        await pending.promise;
      });
      expect(
        screen.queryByDisplayValue("synthetic-stale-value"),
      ).not.toBeInTheDocument();
      expect(
        screen.queryByRole("region", { name: "New API key" }),
      ).not.toBeInTheDocument();
    },
  );
  it("clears an already revealed key when authentication is rechecked", async () => {
    const { rerender } = setup();
    await userEvent.click(
      await screen.findByRole("button", { name: "Create key" }),
    );
    await screen.findByRole("region", { name: "New API key" });
    auth.status = "checking";
    rerender();
    expect(
      screen.queryByDisplayValue("synthetic-one-time-value"),
    ).not.toBeInTheDocument();
  });
  it("does not retry an unknown issuance and leaves revocation voluntary", async () => {
    const user = userEvent.setup();
    const { issue, list, revoke } = setup();
    issue.mockRejectedValue(new Error("synthetic-private-error"));
    await screen.findByText("No API keys yet.");
    list.mockResolvedValue({ items: [key], next_cursor: null });
    await user.click(screen.getByRole("button", { name: "Create key" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Key creation is not confirmed;",
    );
    expect(
      screen.queryByText("synthetic-private-error"),
    ).not.toBeInTheDocument();
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Create key" })).toBeEnabled(),
    );
    expect(issue).toHaveBeenCalledTimes(1);
    expect(revoke).not.toHaveBeenCalled();
    list.mockResolvedValue({
      items: [{ ...key, revoked_at: "2026-01-02T00:00:00Z" }],
      next_cursor: null,
    });
    await user.click(await screen.findByRole("button", { name: "Revoke key" }));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Create key" })).toBeEnabled(),
    );
    expect(revoke).toHaveBeenCalledWith(
      "tenant",
      "project",
      connection.connection_id,
      key.key_id,
    );
    expect(issue).toHaveBeenCalledTimes(1);
  });
  it.each(["loading", "paginated"])(
    "does not infer inaccessible keys from an incomplete %s baseline",
    async (baseline) => {
      const { issue, list, revoke } = setup();
      const pending = deferred<api.PartnerPage<api.PartnerKey>>();
      list.mockReturnValueOnce(
        baseline === "loading"
          ? pending.promise
          : Promise.resolve({ items: [], next_cursor: "synthetic-next-page" }),
      );
      issue.mockRejectedValue(new Error("synthetic-unknown"));
      await screen.findByRole("button", { name: "Create key" });
      if (baseline === "paginated") {
        await screen.findByRole("button", { name: "Load more" });
      }
      list.mockResolvedValue({
        items: [key],
        next_cursor: "synthetic-more-history",
      });
      await userEvent.click(screen.getByRole("button", { name: "Create key" }));
      if (baseline === "loading") {
        await act(async () => {
          pending.resolve({
            items: [key],
            next_cursor: "synthetic-more-history",
          });
          await pending.promise;
        });
      }
      await screen.findByText(/synthetic-key · Active/);
      await waitFor(() =>
        expect(
          screen.getByRole("button", { name: "Create key" }),
        ).toBeEnabled(),
      );
      expect(screen.getByRole("button", { name: "Rotate key" })).toBeEnabled();
      expect(issue).toHaveBeenCalledTimes(1);
      expect(revoke).not.toHaveBeenCalled();
    },
  );
  it("allows deliberate issuance after refreshing an uncertain result with no new key", async () => {
    const { issue } = setup();
    issue.mockRejectedValue(new Error("synthetic-network-error"));
    await userEvent.click(
      await screen.findByRole("button", { name: "Create key" }),
    );
    await screen.findByRole("alert");
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Create key" })).toBeEnabled(),
    );
    expect(issue).toHaveBeenCalledTimes(1);
    expect(
      screen.getByRole("button", { name: "Revoke connection" }),
    ).toBeEnabled();
  });
  it("does not request or expose administration metadata to members", () => {
    auth.role = "member";
    const { connections } = setup();
    expect(
      screen.getByText(/A workspace administrator is required/),
    ).toBeInTheDocument();
    expect(connections).not.toHaveBeenCalled();
    expect(
      screen.queryByRole("button", { name: "Add connection" }),
    ).not.toBeInTheDocument();
  });
});
