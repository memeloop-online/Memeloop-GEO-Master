import { afterEach, beforeEach, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import i18n from "../i18n";
import { ApiError } from "../api/client";
import * as api from "../api/proxyPools";
import { OperatorProxyPools } from "./OperatorProxyPools";

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
vi.mock("../api/proxyPools", () => ({
  listProxyPools: vi.fn(),
  listProxyMembers: vi.fn(),
  createProxyPool: vi.fn(),
  patchProxyPool: vi.fn(),
  createProxyMember: vi.fn(),
  patchProxyMember: vi.fn(),
}));
const pool: api.ProxyPool = {
  pool_id: "pool-a",
  name: "Test pool",
  enabled: true,
  version: 1,
  created_at: "",
  updated_at: "",
};
const member: api.ProxyPoolMember = {
  member_id: "member-a",
  pool_id: "pool-a",
  name: "Test proxy",
  enabled: true,
  version: 2,
  revision_id: "revision-a",
  proxy_server: "http://proxy.example:8080",
  created_at: "",
  updated_at: "",
};
function show() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const tree = () => (
    <FluentProvider theme={webLightTheme}>
      <QueryClientProvider client={client}>
        <OperatorProxyPools />
      </QueryClientProvider>
    </FluentProvider>
  );
  const view = render(tree());
  return { client, ...view, rerenderIdentity: () => view.rerender(tree()) };
}
const fill = (label: string, value: string) =>
  fireEvent.change(screen.getByLabelText(label), { target: { value } });
async function selectPool() {
  fireEvent.click(await screen.findByRole("button", { name: "View proxies" }));
  await screen.findByText(
    "No proxies in this pool. Add a proxy configuration below.",
  );
}
function draft() {
  fill("Proxy name", "New proxy");
  fill("Proxy server", "http://proxy.example:8080");
  fill("Proxy username (optional)", "synthetic-user");
  fill("Proxy password (optional)", "synthetic-secret");
}
beforeEach(async () => {
  vi.resetAllMocks();
  auth.user = "user-a";
  auth.operator = "operator-a";
  auth.role = "resource_admin";
  await i18n.changeLanguage("en");
  vi.mocked(api.listProxyPools).mockResolvedValue({
    items: [pool],
    next_cursor: null,
  });
  vi.mocked(api.listProxyMembers).mockResolvedValue({
    items: [],
    next_cursor: null,
  });
  vi.mocked(api.createProxyMember).mockResolvedValue(member);
  vi.mocked(api.createProxyPool).mockResolvedValue(pool);
  vi.mocked(api.patchProxyPool).mockResolvedValue(pool);
  vi.mocked(api.patchProxyMember).mockResolvedValue(member);
});
afterEach(cleanup);

it("does not call APIs for project administrators", () => {
  auth.role = "tenant_admin";
  show();
  expect(screen.getByText(/Only operator and resource/)).toBeInTheDocument();
  expect(api.listProxyPools).not.toHaveBeenCalled();
  expect(api.listProxyMembers).not.toHaveBeenCalled();
});
it.each(["user", "operator"] as const)(
  "resets drafts and secrets when the authenticated %s changes",
  async (field) => {
    const view = show();
    await selectPool();
    draft();
    fill("Pool name", "Private draft");
    auth[field] = `${field}-b`;
    view.rerenderIdentity();
    expect(screen.getByLabelText("Pool name")).toHaveValue("");
    expect(
      screen.queryByLabelText("Proxy password (optional)"),
    ).not.toBeInTheDocument();
    await waitFor(() => expect(api.listProxyPools).toHaveBeenCalledTimes(2));
    expect(
      view.client
        .getQueryCache()
        .getAll()
        .map((q) => q.queryKey),
    ).toContainEqual(["operator-proxy-pools", auth.user, auth.operator]);
  },
);
it("paginates pools and members using server cursors", async () => {
  vi.mocked(api.listProxyPools).mockImplementation(async (cursor) => ({
    items: cursor
      ? [{ ...pool, pool_id: "pool-b", name: "Second pool" }]
      : [pool],
    next_cursor: cursor ? null : "pool-cursor",
  }));
  vi.mocked(api.listProxyMembers).mockImplementation(
    async (cursorPool, cursor) => ({
      items: [
        {
          ...member,
          member_id: cursor ? "member-b" : "member-a",
          name: cursor ? "Second proxy" : "Test proxy",
          pool_id: cursorPool,
        },
      ],
      next_cursor: cursor ? null : "member-cursor",
    }),
  );
  show();
  fireEvent.click(await screen.findByRole("button", { name: "Load more" }));
  await screen.findByText("Second pool");
  expect(api.listProxyPools).toHaveBeenCalledWith("pool-cursor");
  fireEvent.click(screen.getAllByRole("button", { name: "View proxies" })[0]);
  fireEvent.click(await screen.findByRole("button", { name: "Load more" }));
  await screen.findByText("Second proxy");
  expect(api.listProxyMembers).toHaveBeenCalledWith("pool-a", "member-cursor");
});
it("creates a member once, clears authentication, and keeps secrets out of caches", async () => {
  let resolve!: (value: api.ProxyPoolMember) => void;
  vi.mocked(api.createProxyMember).mockImplementation(
    () =>
      new Promise((done) => {
        resolve = done;
      }),
  );
  const view = show();
  await selectPool();
  draft();
  const button = screen.getByRole("button", { name: "Add proxy" });
  fireEvent.click(button);
  fireEvent.click(button);
  expect(api.createProxyMember).toHaveBeenCalledTimes(1);
  expect(api.createProxyMember).toHaveBeenCalledWith("pool-a", {
    name: "New proxy",
    proxy: {
      server: "http://proxy.example:8080",
      username: "synthetic-user",
      password: "synthetic-secret",
    },
  });
  resolve(member);
  await waitFor(() =>
    expect(screen.getByLabelText("Proxy password (optional)")).toHaveValue(""),
  );
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
it("retains non-secret work on failure and clears credentials", async () => {
  vi.mocked(api.createProxyMember).mockRejectedValue(
    new ApiError(422, {}, "synthetic-secret"),
  );
  show();
  await selectPool();
  draft();
  fireEvent.click(screen.getByRole("button", { name: "Add proxy" }));
  await screen.findByText(/Unable to save/);
  expect(screen.getByLabelText("Proxy name")).toHaveValue("New proxy");
  expect(screen.getByLabelText("Proxy server")).toHaveValue(
    "http://proxy.example:8080",
  );
  expect(screen.getByLabelText("Proxy username (optional)")).toHaveValue("");
  expect(screen.getByLabelText("Proxy password (optional)")).toHaveValue("");
  expect(screen.queryByText("synthetic-secret")).not.toBeInTheDocument();
});
it("halts stale writes and reloads on version conflict without retrying", async () => {
  vi.mocked(api.patchProxyPool).mockRejectedValue(
    new ApiError(409, {}, "stale"),
  );
  show();
  fireEvent.click(await screen.findByRole("button", { name: "Edit" }));
  fill("Pool name", "My revision");
  fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
  await screen.findByText(/The configuration has changed/);
  expect(api.patchProxyPool).toHaveBeenCalledWith("pool-a", {
    expected_version: 1,
    name: "My revision",
    enabled: true,
  });
  expect(screen.getByLabelText("Pool name")).toHaveValue("My revision");
  expect(screen.getByRole("button", { name: "Save changes" })).toBeDisabled();
  await waitFor(() => expect(api.listProxyPools).toHaveBeenCalledTimes(2));
  expect(api.patchProxyPool).toHaveBeenCalledTimes(1);
});
it("edits member metadata without replacing authentication", async () => {
  vi.mocked(api.listProxyMembers).mockResolvedValue({
    items: [member],
    next_cursor: null,
  });
  show();
  fireEvent.click(await screen.findByRole("button", { name: "View proxies" }));
  const row = (await screen.findByText("Test proxy")).closest("li")!;
  fireEvent.click(within(row).getByRole("button", { name: "Edit" }));
  expect(
    screen.queryByLabelText("Proxy password (optional)"),
  ).not.toBeInTheDocument();
  fill("Proxy name", "Renamed");
  fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
  await waitFor(() =>
    expect(api.patchProxyMember).toHaveBeenCalledWith("pool-a", "member-a", {
      expected_version: 2,
      name: "Renamed",
      enabled: true,
    }),
  );
});
it.each([0, 201])(
  "blocks resubmission after an unknown create result (%s)",
  async (status) => {
    vi.mocked(api.createProxyPool).mockRejectedValue(
      new ApiError(status, {}, "unreadable response"),
    );
    show();
    fill("Pool name", "New pool");
    fireEvent.click(screen.getByRole("button", { name: "Create pool" }));
    await screen.findByText(/Creation is not confirmed/);
    expect(screen.getByRole("button", { name: "Create pool" })).toBeDisabled();
    expect(api.createProxyPool).toHaveBeenCalledTimes(1);
  },
);

it("replaces a member proxy explicitly and clears its authentication", async () => {
  vi.mocked(api.listProxyMembers).mockResolvedValue({
    items: [member],
    next_cursor: null,
  });
  show();
  fireEvent.click(await screen.findByRole("button", { name: "View proxies" }));
  const row = (await screen.findByText("Test proxy")).closest("li")!;
  fireEvent.click(within(row).getByRole("button", { name: "Edit" }));
  fireEvent.click(
    screen.getByRole("checkbox", { name: "Replace proxy configuration" }),
  );
  fill("Proxy username (optional)", "synthetic-user");
  fill("Proxy password (optional)", "synthetic-secret");
  fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
  await waitFor(() =>
    expect(api.patchProxyMember).toHaveBeenCalledWith("pool-a", "member-a", {
      expected_version: 2,
      name: "Test proxy",
      enabled: true,
      proxy: {
        server: member.proxy_server,
        username: "synthetic-user",
        password: "synthetic-secret",
      },
    }),
  );
  await waitFor(() =>
    expect(screen.getByLabelText("Proxy password (optional)")).toHaveValue(""),
  );
});

it("rejects embedded credentials without sending or retaining authentication", async () => {
  show();
  await selectPool();
  draft();
  fill(
    "Proxy server",
    "http://synthetic-user:synthetic-secret@proxy.example:8080",
  );
  fireEvent.click(screen.getByRole("button", { name: "Add proxy" }));
  expect(api.createProxyMember).not.toHaveBeenCalled();
  expect(screen.getByLabelText("Proxy password (optional)")).toHaveValue("");
  expect(screen.getByLabelText("Proxy server")).toHaveValue("");
  expect(screen.getByText(/Enter an HTTP/)).toBeInTheDocument();
});

it.each([
  "http://proxy.example:80",
  "https://proxy.example:443",
  "http://proxy.example",
  "https://proxy.example",
])("accepts effective default HTTP(S) ports: %s", async (server) => {
  show();
  await selectPool();
  draft();
  fill("Proxy server", server);
  fireEvent.click(screen.getByRole("button", { name: "Add proxy" }));
  await waitFor(() =>
    expect(api.createProxyMember).toHaveBeenCalledWith("pool-a", {
      name: "New proxy",
      proxy: {
        server,
        username: "synthetic-user",
        password: "synthetic-secret",
      },
    }),
  );
});

it.each([
  "http://proxy.example:0",
  "https://proxy.example:0",
  "socks5://proxy.example:0",
  "socks5://proxy.example",
])("rejects invalid proxy ports: %s", async (server) => {
  show();
  await selectPool();
  draft();
  fill("Proxy server", server);
  fireEvent.click(screen.getByRole("button", { name: "Add proxy" }));
  expect(api.createProxyMember).not.toHaveBeenCalled();
  expect(screen.getByText(/Enter an HTTP/)).toBeInTheDocument();
  expect(screen.getByLabelText("Proxy password (optional)")).toHaveValue("");
});
