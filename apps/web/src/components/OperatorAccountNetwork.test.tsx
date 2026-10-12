import { beforeEach, afterEach, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import i18n from "../i18n";
import { ApiError } from "../api/client";
import * as api from "../api/accountNetworks";
import * as inventory from "../api/proxyPools";
import { OperatorAccountNetwork } from "./OperatorAccountNetwork";

const auth = vi.hoisted(() => ({
  user: "user-a",
  operator: "operator-a",
  role: "resource_admin",
}));
vi.mock("../auth/AuthProvider", () => ({
  useAuth: () => ({
    session: {
      user: { id: auth.user },
      operator: { id: auth.operator },
      memberships: [{ role: auth.role }],
    },
  }),
}));
vi.mock("../api/accountNetworks", () => ({
  getAccountNetwork: vi.fn(),
  saveAccountNetwork: vi.fn(),
}));
vi.mock("../api/proxyPools", () => ({
  listProxyPools: vi.fn(),
  listProxyMembers: vi.fn(),
}));
const direct: api.SavedNetwork = {
  binding_id: "binding-a",
  source: { kind: "direct" },
  server: null,
};
const saved: api.AccountNetwork = {
  account_id: "account-a",
  version: 0,
  active: {
    binding_id: null,
    source: { kind: "legacy_snapshot" },
    server: null,
  },
  pending: null,
  busy: false,
};
const pool = {
  pool_id: "pool-a",
  name: "Pool A",
  enabled: true,
  version: 1,
  created_at: "",
  updated_at: "",
};
const member = {
  pool_id: "pool-a",
  member_id: "member-a",
  revision_id: "revision-a",
  name: "Proxy A",
  enabled: true,
  version: 1,
  proxy_server: "http://proxy.example:8080",
  created_at: "",
  updated_at: "",
};
function show(accountId = "account-a") {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const tree = (id: string) => (
    <FluentProvider theme={webLightTheme}>
      <QueryClientProvider client={client}>
        <OperatorAccountNetwork accountId={id} />
      </QueryClientProvider>
    </FluentProvider>
  );
  const view = render(tree(accountId));
  return {
    client,
    ...view,
    changeIdentity: (id = accountId) => view.rerender(tree(id)),
  };
}
async function open() {
  fireEvent.click(screen.getByText("Account network"));
  await screen.findByLabelText("Network choice");
  await waitFor(() => expect(api.getAccountNetwork).toHaveBeenCalled());
}
const fill = (label: string, value: string) =>
  fireEvent.change(screen.getByLabelText(label), { target: { value } });
async function pick() {
  await screen.findByRole("option", { name: "Pool A" });
  fill("Proxy pool", "pool-a");
  await screen.findByRole("option", { name: "Proxy A" });
  fill("Proxy", "member-a:revision-a");
}
function manual() {
  fill("Network choice", "standalone");
  fill("Proxy server", "http://proxy.example:80");
  fill("Proxy username (optional)", "synthetic-user");
  fill("Proxy password (optional)", "synthetic-secret");
}
beforeEach(async () => {
  vi.resetAllMocks();
  auth.user = "user-a";
  auth.operator = "operator-a";
  auth.role = "resource_admin";
  await i18n.changeLanguage("en");
  vi.mocked(api.getAccountNetwork).mockResolvedValue(saved);
  vi.mocked(api.saveAccountNetwork).mockResolvedValue({ ...saved, version: 1 });
  vi.mocked(inventory.listProxyPools).mockResolvedValue({
    items: [pool],
    next_cursor: null,
  });
  vi.mocked(inventory.listProxyMembers).mockResolvedValue({
    items: [member],
    next_cursor: null,
  });
});
afterEach(cleanup);

it("does not load network or pool inventory for unauthorized users or collapsed details", () => {
  const view = show();
  expect(api.getAccountNetwork).not.toHaveBeenCalled();
  expect(inventory.listProxyPools).not.toHaveBeenCalled();
  auth.role = "tenant_admin";
  view.changeIdentity();
  expect(screen.queryByText("Account network")).not.toBeInTheDocument();
  expect(api.getAccountNetwork).not.toHaveBeenCalled();
});
it("sends the selected immutable revision with the observed version zero", async () => {
  show();
  await open();
  await pick();
  fireEvent.click(screen.getByRole("button", { name: "Save network" }));
  await screen.findByText("Network saved.");
  expect(api.saveAccountNetwork).toHaveBeenCalledExactlyOnceWith("account-a", {
    expected_version: 0,
    source: {
      kind: "pool_revision",
      pool_id: "pool-a",
      member_id: "member-a",
      revision_id: "revision-a",
    },
  });
});
it("shows backend pending separately from the still-active network", async () => {
  vi.mocked(api.saveAccountNetwork).mockResolvedValue({
    ...saved,
    version: 1,
    active: direct,
    busy: true,
    pending: {
      binding_id: "binding-b",
      member_name: "Proxy A",
      source: {
        kind: "pool_revision",
        pool_id: "pool-a",
        member_id: "member-a",
        revision_id: "revision-a",
      },
      server: member.proxy_server,
    },
  });
  show();
  await open();
  await pick();
  fireEvent.click(screen.getByRole("button", { name: "Save network" }));
  await screen.findByText(
    "Network saved. It will take effect after the current browser closes.",
  );
  expect(
    screen.getByText("Current network: Default network"),
  ).toBeInTheDocument();
  expect(screen.getByText(/Next network: Proxy A/)).toBeInTheDocument();
});
it("clears manual secrets after failure while retaining the server", async () => {
  vi.mocked(api.saveAccountNetwork).mockRejectedValue(
    new ApiError(400, {}, "synthetic-secret"),
  );
  const view = show();
  await open();
  manual();
  fireEvent.click(screen.getByRole("button", { name: "Save network" }));
  await screen.findByText(/Unable to save/);
  expect(screen.getByLabelText("Proxy server")).toHaveValue(
    "http://proxy.example:80",
  );
  expect(screen.getByLabelText("Proxy password (optional)")).toHaveValue("");
  expect(screen.getByLabelText("Proxy username (optional)")).toHaveValue("");
  expect(view.client.getMutationCache().getAll()).toEqual([]);
  expect(
    JSON.stringify(
      view.client
        .getQueryCache()
        .getAll()
        .map((q) => q.state.data),
    ),
  ).not.toContain("synthetic-secret");
});
it("saves standalone and direct choices through the real binding adapter", async () => {
  show();
  await open();
  manual();
  fireEvent.click(screen.getByRole("button", { name: "Save network" }));
  await screen.findByText("Network saved.");
  expect(api.saveAccountNetwork).toHaveBeenLastCalledWith("account-a", {
    expected_version: 0,
    source: {
      kind: "standalone",
      proxy: {
        server: "http://proxy.example:80",
        username: "synthetic-user",
        password: "synthetic-secret",
      },
    },
  });
  expect(screen.getByLabelText("Proxy password (optional)")).toHaveValue("");
  fill("Network choice", "direct");
  fireEvent.click(screen.getByRole("button", { name: "Save network" }));
  await waitFor(() =>
    expect(api.saveAccountNetwork).toHaveBeenLastCalledWith("account-a", {
      expected_version: 1,
      source: { kind: "direct" },
    }),
  );
});
it.each(["user", "operator", "account"] as const)(
  "resets open state and secrets on %s changes",
  async (field) => {
    const view = show();
    await open();
    manual();
    if (field !== "account") auth[field] = `${field}-b`;
    view.changeIdentity(field === "account" ? "account-b" : "account-a");
    expect(
      screen.queryByLabelText("Proxy password (optional)"),
    ).not.toBeInTheDocument();
    await open();
    fill("Network choice", "standalone");
    expect(screen.getByLabelText("Proxy password (optional)")).toHaveValue("");
    expect(screen.getByLabelText("Proxy server")).toHaveValue("");
  },
);
it.each([409, 0, 200, 500])(
  "requires readback after conflict or uncertain response %s",
  async (status) => {
    vi.mocked(api.saveAccountNetwork).mockRejectedValue(
      new ApiError(status, {}, "unavailable"),
    );
    show();
    await open();
    await pick();
    fireEvent.click(screen.getByRole("button", { name: "Save network" }));
    await screen.findByText(
      status === 409 ? /The network changed/ : /The result is not confirmed/,
    );
    expect(screen.getByRole("button", { name: "Save network" })).toBeDisabled();
    expect(api.saveAccountNetwork).toHaveBeenCalledTimes(1);
    vi.mocked(api.getAccountNetwork).mockResolvedValue({
      ...saved,
      version: 3,
    });
    fireEvent.click(screen.getByRole("button", { name: "Reload" }));
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Save network" }),
      ).not.toBeDisabled(),
    );
    fireEvent.click(screen.getByRole("button", { name: "Save network" }));
    await waitFor(() =>
      expect(api.saveAccountNetwork).toHaveBeenLastCalledWith(
        "account-a",
        expect.objectContaining({ expected_version: 3 }),
      ),
    );
  },
);
it("does not silently select a rotated member revision", async () => {
  show();
  await open();
  await pick();
  vi.mocked(inventory.listProxyMembers).mockResolvedValue({
    items: [{ ...member, revision_id: "revision-b" }],
    next_cursor: null,
  });
  fireEvent.click(screen.getByRole("button", { name: "Reload" }));
  await waitFor(() =>
    expect(inventory.listProxyMembers).toHaveBeenCalledTimes(2),
  );
  expect(screen.getByRole("button", { name: "Save network" })).toBeDisabled();
  expect(api.saveAccountNetwork).not.toHaveBeenCalled();
});
it("loads additional pools and members without allowing disabled choices", async () => {
  vi.mocked(inventory.listProxyPools).mockImplementation(async (cursor) => ({
    items: cursor
      ? [pool]
      : [
          {
            ...pool,
            pool_id: "disabled",
            enabled: false,
            name: "Disabled pool",
          },
        ],
    next_cursor: cursor ? null : "next-pool",
  }));
  vi.mocked(inventory.listProxyMembers).mockImplementation(
    async (_, cursor) => ({
      items: cursor ? [member] : [],
      next_cursor: cursor ? null : "next-member",
    }),
  );
  show();
  await open();
  expect(
    await screen.findByRole("option", { name: "Disabled pool (Disabled)" }),
  ).toBeDisabled();
  fireEvent.click(screen.getByRole("button", { name: "Load more" }));
  await screen.findByRole("option", { name: "Pool A" });
  fill("Proxy pool", "pool-a");
  fireEvent.click(await screen.findByRole("button", { name: "Load more" }));
  await screen.findByRole("option", { name: "Proxy A" });
  expect(inventory.listProxyPools).toHaveBeenCalledWith("next-pool");
  expect(inventory.listProxyMembers).toHaveBeenCalledWith(
    "pool-a",
    "next-member",
  );
});

it("ignores a late save after changing accounts and prevents double submission", async () => {
  let complete!: (result: api.AccountNetwork) => void;
  vi.mocked(api.saveAccountNetwork).mockImplementation(
    () =>
      new Promise((resolve) => {
        complete = resolve;
      }),
  );
  const view = show();
  await open();
  manual();
  const button = screen.getByRole("button", { name: "Save network" });
  fireEvent.click(button);
  fireEvent.click(button);
  expect(api.saveAccountNetwork).toHaveBeenCalledTimes(1);
  view.changeIdentity("account-b");
  complete({ ...saved, version: 1 });
  await waitFor(() =>
    expect(screen.queryByText("Network saved.")).not.toBeInTheDocument(),
  );
  expect(
    view.client.getQueryData([
      "operator-account-network",
      "user-a",
      "operator-a",
      "account-b",
    ]),
  ).toBeUndefined();
});

it("does not report success for an empty accepted response", async () => {
  vi.mocked(api.saveAccountNetwork).mockResolvedValue(
    undefined as unknown as api.AccountNetwork,
  );
  show();
  await open();
  await pick();
  fireEvent.click(screen.getByRole("button", { name: "Save network" }));
  await screen.findByText(/The result is not confirmed/);
  expect(screen.queryByText("Network saved.")).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Save network" })).toBeDisabled();
});

it("does not silently advance a draft's version after background query refresh", async () => {
  const view = show();
  await open();
  await pick();
  vi.mocked(api.getAccountNetwork).mockResolvedValue({ ...saved, version: 5 });
  await view.client.invalidateQueries({
    queryKey: ["operator-account-network"],
  });
  fireEvent.click(screen.getByRole("button", { name: "Save network" }));
  await waitFor(() =>
    expect(api.saveAccountNetwork).toHaveBeenCalledWith(
      "account-a",
      expect.objectContaining({ expected_version: 0 }),
    ),
  );
});

it("restores saved names and distinguishes same-server configurations independently of the draft", async () => {
  const active: api.SavedNetwork = {
    binding_id: "binding-old",
    source: {
      kind: "pool_revision",
      pool_id: "saved-pool",
      member_id: "saved-member",
      revision_id: "12345678-aaaa",
    },
    pool_name: "Saved pool",
    member_name: "Saved proxy",
    server: member.proxy_server,
  };
  vi.mocked(api.getAccountNetwork).mockResolvedValue({
    ...saved,
    version: 2,
    busy: true,
    active,
    pending: {
      ...active,
      binding_id: "binding-new",
      source: { ...active.source, revision_id: "12345678-bbbb" },
    },
  });
  show();
  await open();
  expect(
    await screen.findByText(/Current network: Saved pool · Saved proxy/),
  ).toBeInTheDocument();
  expect(
    screen.getByText(/Next network: Saved pool · Saved proxy/),
  ).toBeInTheDocument();
  expect(screen.getByText("Configuration 12345678-a")).toHaveAttribute(
    "title",
    "12345678-aaaa",
  );
  expect(screen.getByText("Configuration 12345678-b")).toHaveAttribute(
    "title",
    "12345678-bbbb",
  );
  // The saved pool is not on the first inventory page. Its identity does not
  // depend on browsing that page or selecting a different proxy in the draft.
  expect(inventory.listProxyMembers).not.toHaveBeenCalled();
  await pick();
  expect(
    screen.getByText(/Current network: Saved pool · Saved proxy/),
  ).toBeInTheDocument();
  expect(
    screen.getByText(/Next network: Saved pool · Saved proxy/),
  ).toBeInTheDocument();
  expect(screen.getByText("Configuration 12345678-a")).toBeInTheDocument();
  expect(inventory.listProxyMembers).toHaveBeenCalledExactlyOnceWith(
    "pool-a",
    undefined,
  );
});
