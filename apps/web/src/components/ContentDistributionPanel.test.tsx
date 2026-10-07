import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import type { ContentRevision } from "../api/content";
import type {
  ChannelAccount,
  ChannelPlatform,
  ProjectConnectorCapability,
} from "../api/channels";
import {
  ContentDistributionPanel,
  eligiblePublishingAccounts,
} from "./ContentDistributionPanel";
import {
  getContentDistributionPublication,
  getContentDistributionRequest,
  submitContentDistributionRequest,
} from "../api/contentDistribution";

vi.mock("../auth/AuthProvider", () => ({
  useAuth: () => ({
    session: {
      user: { id: "user-1" },
      operator: { id: "operator-1" },
      memberships: [{ tenant_id: "tenant-1", role: "member" }],
    },
  }),
}));
vi.mock("../api/channels", () => ({
  listChannelAccounts: vi.fn(),
  listChannelPlatforms: vi.fn(),
  listProjectConnectorCapabilities: vi.fn(),
}));
vi.mock("../api/contentDistribution", async (original) => ({
  ...(await original<typeof import("../api/contentDistribution")>()),
  submitContentDistributionRequest: vi.fn(),
  getContentDistributionRequest: vi.fn(),
  getContentDistributionPublication: vi.fn(),
}));
import {
  listChannelAccounts,
  listChannelPlatforms,
  listProjectConnectorCapabilities,
} from "../api/channels";

const revision: ContentRevision = {
  revision_id: "revision-1",
  asset_id: "asset-1",
  revision: 1,
  base_revision_id: null,
  document: {
    title: "Saved article",
    blocks: [
      {
        block_id: "block-1",
        kind: "paragraph",
        text: "text",
        items: [],
        citation_ids: [],
      },
    ],
  },
  markdown: "text",
  evidence: [],
  quotes: [],
  findings: [],
  created_at: "2026-01-01T00:00:00Z",
};
const accounts: ChannelAccount[] = [
  {
    account_id: "own-1",
    project_id: "project-1",
    platform: "zhihu",
    group_id: null,
    status: "ready",
    display_name: "Publishing account",
    platform_account_id: null,
    avatar_url: null,
    enabled: true,
    proxy_configured: false,
    proxy_server: null,
    created_at: "",
    updated_at: "",
    owner_kind: "customer",
  },
  {
    account_id: "shared-1",
    project_id: "project-1",
    platform: "zhihu",
    group_id: null,
    status: "ready",
    display_name: "Shared account",
    platform_account_id: null,
    avatar_url: null,
    enabled: true,
    proxy_configured: false,
    proxy_server: null,
    created_at: "",
    updated_at: "",
    owner_kind: "operator_pool",
  },
  {
    account_id: "measurement-1",
    project_id: "project-1",
    platform: "kimi",
    group_id: null,
    status: "ready",
    display_name: "Search account",
    platform_account_id: null,
    avatar_url: null,
    enabled: true,
    proxy_configured: false,
    proxy_server: null,
    created_at: "",
    updated_at: "",
  },
];
const platforms: ChannelPlatform[] = [
  {
    id: "zhihu",
    label: "Publisher",
    purpose: "publishing",
    login_supported: true,
  },
  {
    id: "kimi",
    label: "Search",
    purpose: "measurement",
    login_supported: true,
  },
];
const capabilities: ProjectConnectorCapability[] = [
  {
    platform_id: "zhihu",
    placement_slot: "primary",
    revision: 1,
    enabled: true,
    availability: "available",
    content_types: ["plain_text_article.v1"],
  },
];
const receipt = {
  request_id: "request-1",
  scope: {
    project_id: "project-1",
    tenant_id: "tenant-1",
    operator_id: "operator-1",
  },
  content_asset_id: "asset-1",
  content_revision_id: "revision-1",
  account_id: "own-1",
  placement_slot: "primary",
  format: "markdown.v1",
  publication_intent_id: null,
};

function Path() {
  const location = useLocation();
  return <output data-testid="path">{location.search}</output>;
}
function mount({
  path = "/?reuse_item_id=item-1",
  selectedRevision = revision,
  projectId = "project-1",
  assetId = "asset-1",
  onSelectRevision,
}: {
  path?: string;
  selectedRevision?: ContentRevision;
  projectId?: string;
  assetId?: string;
  onSelectRevision?: (revisionId: string) => void;
} = {}) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <MemoryRouter initialEntries={[path]}>
      <QueryClientProvider client={client}>
        <FluentProvider theme={webLightTheme}>
          <Routes>
            <Route
              path="/"
              element={
                <>
                  <ContentDistributionPanel
                    tenantId="tenant-1"
                    projectId={projectId}
                    assetId={assetId}
                    revision={selectedRevision}
                    readonly={false}
                    unsaved={false}
                    onSelectRevision={onSelectRevision}
                  />
                  <Path />
                </>
              }
            />
          </Routes>
        </FluentProvider>
      </QueryClientProvider>
    </MemoryRouter>,
  );
}
beforeEach(() => {
  vi.clearAllMocks();
  window.sessionStorage.clear();
  vi.mocked(listChannelAccounts).mockResolvedValue({ items: accounts });
  vi.mocked(listChannelPlatforms).mockResolvedValue({ items: platforms });
  vi.mocked(listProjectConnectorCapabilities).mockResolvedValue({
    items: capabilities,
  });
  vi.mocked(getContentDistributionRequest).mockResolvedValue(receipt as never);
  vi.mocked(getContentDistributionPublication).mockResolvedValue({
    request_id: "request-1",
    publication_intent_id: null,
    channel_target_id: null,
    attempt_id: null,
    outcome: null,
    public_url: null,
    fixture: null,
  });
  vi.mocked(submitContentDistributionRequest).mockResolvedValue(
    receipt as never,
  );
});

describe("ContentDistributionPanel", () => {
  it("filters measurement, disconnected, disabled, and unsupported accounts", () => {
    expect(
      eligiblePublishingAccounts(
        [
          ...accounts,
          { ...accounts[0], account_id: "expired", status: "expired" },
        ],
        platforms,
        capabilities,
        revision,
      ).map((account) => account.account_id),
    ).toEqual(["own-1", "shared-1"]);
    expect(
      eligiblePublishingAccounts(accounts, platforms, capabilities, {
        ...revision,
        document: { ...revision.document, schema_version: 2 },
      }),
    ).toEqual([]);
  });

  it("submits one click and reuses the exact key and revision on a network retry", async () => {
    vi.mocked(submitContentDistributionRequest)
      .mockRejectedValueOnce(new Error("Connection lost"))
      .mockResolvedValueOnce(receipt as never);
    const user = userEvent.setup();
    mount();
    await user.selectOptions(await screen.findByLabelText("发布账号"), "own-1");
    await user.click(screen.getByRole("button", { name: "发布所选版本" }));
    await screen.findByText("提交结果暂未确认，请重试。");
    await user.click(screen.getByRole("button", { name: "重试" }));
    await waitFor(() =>
      expect(submitContentDistributionRequest).toHaveBeenCalledTimes(2),
    );
    expect(vi.mocked(submitContentDistributionRequest).mock.calls[0]).toEqual(
      vi.mocked(submitContentDistributionRequest).mock.calls[1],
    );
    expect(
      vi.mocked(submitContentDistributionRequest).mock.calls[0]?.[2],
    ).toMatchObject({
      content_asset_id: "asset-1",
      content_revision_id: "revision-1",
      account_id: "own-1",
      placement_slot: "primary",
      format: "markdown.v1",
    });
    expect(await screen.findByText("发布请求已提交")).toBeTruthy();
    expect(screen.getByTestId("path").textContent).toContain(
      "distribution_request_id=request-1",
    );
    expect(window.sessionStorage.length).toBe(0);
  });

  it("restores the exact uncertain request across remount even when the selected revision advances", async () => {
    vi.mocked(submitContentDistributionRequest).mockRejectedValue(
      new Error("Response lost"),
    );
    const user = userEvent.setup();
    const original = mount();
    await user.selectOptions(await screen.findByLabelText("发布账号"), "own-1");
    await user.click(screen.getByRole("button", { name: "发布所选版本" }));
    await screen.findByRole("button", { name: "重试" });
    expect(window.sessionStorage.length).toBe(1);
    const first = vi.mocked(submitContentDistributionRequest).mock.calls[0];
    original.unmount();
    mount({
      selectedRevision: {
        ...revision,
        revision_id: "revision-2",
        revision: 2,
        document: { ...revision.document, schema_version: 2 },
      },
    });
    expect(screen.queryByRole("button", { name: "发布所选版本" })).toBeNull();
    expect(screen.queryByLabelText("发布账号")).toBeNull();
    expect(submitContentDistributionRequest).toHaveBeenCalledTimes(1);
    await user.click(screen.getByRole("button", { name: "重试" }));
    await waitFor(() =>
      expect(submitContentDistributionRequest).toHaveBeenCalledTimes(2),
    );
    expect(vi.mocked(submitContentDistributionRequest).mock.calls[1]).toEqual(
      first,
    );
    expect(window.sessionStorage.length).toBe(1);
  });

  it("keeps pending requests isolated from other projects and assets", async () => {
    vi.mocked(submitContentDistributionRequest).mockRejectedValue(
      new Error("Response lost"),
    );
    const user = userEvent.setup();
    const original = mount();
    await user.selectOptions(await screen.findByLabelText("发布账号"), "own-1");
    await user.click(screen.getByRole("button", { name: "发布所选版本" }));
    await screen.findByRole("button", { name: "重试" });
    original.unmount();
    const differentProject = mount({ projectId: "project-2" });
    expect(
      await screen.findByRole("button", { name: "发布所选版本" }),
    ).toBeTruthy();
    expect(screen.queryByRole("button", { name: "重试" })).toBeNull();
    differentProject.unmount();
    const differentAsset = mount({
      assetId: "asset-2",
      selectedRevision: { ...revision, asset_id: "asset-2" },
    });
    expect(
      await screen.findByRole("button", { name: "发布所选版本" }),
    ).toBeTruthy();
    expect(screen.queryByRole("button", { name: "重试" })).toBeNull();
    differentAsset.unmount();
    mount();
    expect(screen.getByRole("button", { name: "重试" })).toBeTruthy();
    expect(window.sessionStorage.length).toBe(1);
  });

  it("ignores malformed pending session data instead of replaying it", async () => {
    window.sessionStorage.setItem(
      'content-distribution-pending:["user-1","operator-1","tenant-1","project-1","asset-1"]',
      JSON.stringify({
        key: "old-key",
        input: {
          content_asset_id: "asset-other",
          content_revision_id: "rev-1",
          account_id: "account-1",
          placement_slot: "primary",
          format: "markdown.v1",
        },
      }),
    );
    mount();
    expect(
      await screen.findByRole("button", { name: "发布所选版本" }),
    ).toBeTruthy();
    expect(screen.queryByRole("button", { name: "重试" })).toBeNull();
    expect(submitContentDistributionRequest).not.toHaveBeenCalled();
  });

  it("changing account, revision, or project creates a new key", async () => {
    vi.mocked(submitContentDistributionRequest).mockRejectedValue(
      new Error("Network"),
    );
    const user = userEvent.setup();
    const mounted = mount();
    await user.selectOptions(await screen.findByLabelText("发布账号"), "own-1");
    await user.click(screen.getByRole("button", { name: "发布所选版本" }));
    await screen.findByRole("button", { name: "重试" });
    await user.click(screen.getByRole("button", { name: "选择其他发布账号" }));
    await user.selectOptions(screen.getByLabelText("发布账号"), "shared-1");
    await user.click(screen.getByRole("button", { name: "发布所选版本" }));
    await waitFor(() =>
      expect(submitContentDistributionRequest).toHaveBeenCalledTimes(2),
    );
    expect(
      vi.mocked(submitContentDistributionRequest).mock.calls[0]?.[3],
    ).not.toBe(vi.mocked(submitContentDistributionRequest).mock.calls[1]?.[3]);
    mounted.unmount();
    const next = mount({
      selectedRevision: { ...revision, revision_id: "revision-2", revision: 2 },
    });
    await user.click(screen.getByRole("button", { name: "选择其他发布账号" }));
    await user.selectOptions(await screen.findByLabelText("发布账号"), "own-1");
    await user.click(screen.getByRole("button", { name: "发布所选版本" }));
    await waitFor(() =>
      expect(submitContentDistributionRequest).toHaveBeenCalledTimes(3),
    );
    expect(
      vi.mocked(submitContentDistributionRequest).mock.calls[2]?.[2]
        .content_revision_id,
    ).toBe("revision-2");
    expect(
      vi.mocked(submitContentDistributionRequest).mock.calls[2]?.[3],
    ).not.toBe(vi.mocked(submitContentDistributionRequest).mock.calls[1]?.[3]);
    next.unmount();
    mount({ projectId: "project-2" });
    await user.selectOptions(await screen.findByLabelText("发布账号"), "own-1");
    await user.click(screen.getByRole("button", { name: "发布所选版本" }));
    await waitFor(() =>
      expect(submitContentDistributionRequest).toHaveBeenCalledTimes(4),
    );
    expect(vi.mocked(submitContentDistributionRequest).mock.calls[3]?.[1]).toBe(
      "project-2",
    );
    expect(
      vi.mocked(submitContentDistributionRequest).mock.calls[3]?.[3],
    ).not.toBe(vi.mocked(submitContentDistributionRequest).mock.calls[2]?.[3]);
  });

  it("blocks rich revisions without matching format capability", async () => {
    const rich = {
      ...revision,
      document: { ...revision.document, schema_version: 2 as const },
    };
    mount({ selectedRevision: rich });
    expect(
      await screen.findByText("此版本的格式尚不能发布到已连接账号。"),
    ).toBeTruthy();
    expect(screen.queryByRole("button", { name: "发布所选版本" })).toBeNull();
    expect(submitContentDistributionRequest).not.toHaveBeenCalled();
  });

  it("a URL receipt reload GETs without POST and does not claim delivery", async () => {
    mount({ path: "/?distribution_request_id=request-1" });
    expect(await screen.findByText("发布请求已提交")).toBeTruthy();
    expect(getContentDistributionRequest).toHaveBeenCalledWith(
      "tenant-1",
      "project-1",
      "request-1",
    );
    expect(getContentDistributionPublication).toHaveBeenCalledWith(
      "tenant-1",
      "project-1",
      "request-1",
    );
    expect(await screen.findByText("等待发布")).toBeTruthy();
    expect(submitContentDistributionRequest).not.toHaveBeenCalled();
  });

  it("clears a recovered pending key after loading the matching persisted receipt", async () => {
    window.sessionStorage.setItem(
      'content-distribution-pending:["user-1","operator-1","tenant-1","project-1","asset-1"]',
      JSON.stringify({
        key: "same-key",
        input: {
          content_asset_id: "asset-1",
          content_revision_id: "revision-1",
          account_id: "own-1",
          placement_slot: "primary",
          format: "markdown.v1",
        },
      }),
    );
    mount({ path: "/?distribution_request_id=request-1" });
    expect(await screen.findByText("发布请求已提交")).toBeTruthy();
    await waitFor(() => expect(window.sessionStorage.length).toBe(0));
    expect(submitContentDistributionRequest).not.toHaveBeenCalled();
  });

  it("does not discard an unrelated uncertain request when an older receipt is opened", async () => {
    window.sessionStorage.setItem(
      'content-distribution-pending:["user-1","operator-1","tenant-1","project-1","asset-1"]',
      JSON.stringify({
        key: "pending-new-revision",
        input: {
          content_asset_id: "asset-1",
          content_revision_id: "revision-2",
          account_id: "own-1",
          placement_slot: "primary",
          format: "markdown.v1",
        },
      }),
    );
    mount({ path: "/?distribution_request_id=request-1" });
    expect(await screen.findByText("发布请求已提交")).toBeTruthy();
    expect(window.sessionStorage.length).toBe(1);
    expect(submitContentDistributionRequest).not.toHaveBeenCalled();
  });

  it("does not send if browser session storage cannot preserve the retry key", async () => {
    const originalStorage = window.sessionStorage;
    Object.defineProperty(window, "sessionStorage", {
      configurable: true,
      value: {
        getItem: () => null,
        setItem: () => {
          throw new Error("storage blocked");
        },
        removeItem: () => {},
      },
    });
    try {
      const user = userEvent.setup();
      mount();
      await user.selectOptions(
        await screen.findByLabelText("发布账号"),
        "own-1",
      );
      await user.click(screen.getByRole("button", { name: "发布所选版本" }));
      expect(
        await screen.findByText("提交结果暂未确认，请重试。"),
      ).toBeTruthy();
      expect(submitContentDistributionRequest).not.toHaveBeenCalled();
    } finally {
      Object.defineProperty(window, "sessionStorage", {
        configurable: true,
        value: originalStorage,
      });
    }
  });

  it("an old receipt stays bound to its frozen revision", async () => {
    const select = vi.fn();
    mount({
      path: "/?distribution_request_id=request-1",
      selectedRevision: { ...revision, revision_id: "revision-2", revision: 2 },
      onSelectRevision: select,
    });
    expect(
      await screen.findByText(
        "当前所选版本与本次请求不同。以下状态只属于请求中固定的版本。",
      ),
    ).toBeTruthy();
    expect(screen.getByText("请求版本")).toBeTruthy();
    expect(screen.getByText("revision-1")).toBeTruthy();
    expect(screen.queryByText("所选版本 v2")).toBeNull();
    await userEvent
      .setup()
      .click(screen.getByRole("button", { name: "查看请求的版本" }));
    expect(select).toHaveBeenCalledWith("revision-1");
    expect(submitContentDistributionRequest).not.toHaveBeenCalled();
  });

  it("refreshes the ledger projection and distinguishes published from verified", async () => {
    vi.mocked(getContentDistributionPublication)
      .mockResolvedValueOnce({
        request_id: "request-1",
        publication_intent_id: "intent-1",
        channel_target_id: "target-1",
        attempt_id: "attempt-1",
        outcome: "published",
        public_url: null,
        fixture: false,
      })
      .mockResolvedValueOnce({
        request_id: "request-1",
        publication_intent_id: "intent-1",
        channel_target_id: "target-1",
        attempt_id: "attempt-1",
        outcome: "verified",
        public_url: null,
        fixture: false,
      });
    mount({ path: "/?distribution_request_id=request-1" });
    expect(
      await screen.findByText("平台已接收；公开验证尚未确认。"),
    ).toBeTruthy();
    await userEvent
      .setup()
      .click(screen.getByRole("button", { name: "刷新发布进度" }));
    expect(await screen.findByText("已完成公开验证。")).toBeTruthy();
    expect(submitContentDistributionRequest).not.toHaveBeenCalled();
  });

  it("does not describe a fixture outcome as a real publication", async () => {
    vi.mocked(getContentDistributionPublication).mockResolvedValue({
      request_id: "request-1",
      publication_intent_id: "intent-1",
      channel_target_id: "target-1",
      attempt_id: "attempt-1",
      outcome: "verified",
      fixture: true,
      public_url: null,
    });
    mount({ path: "/?distribution_request_id=request-1" });
    expect(await screen.findByText("测试结果")).toBeTruthy();
    expect(screen.queryByText("已完成公开验证。")).toBeNull();
  });

  it("shows an unknown outcome without claiming reconciliation has started", async () => {
    vi.mocked(getContentDistributionPublication).mockResolvedValue({
      request_id: "request-1",
      publication_intent_id: "intent-1",
      channel_target_id: "target-1",
      attempt_id: "attempt-1",
      outcome: "unknown",
      fixture: false,
      public_url: null,
    });
    mount({ path: "/?distribution_request_id=request-1" });
    expect(await screen.findByText("发布结果待确认")).toBeTruthy();
    expect(screen.queryByText(/正在查回|请勿重发/)).toBeNull();
  });
});
