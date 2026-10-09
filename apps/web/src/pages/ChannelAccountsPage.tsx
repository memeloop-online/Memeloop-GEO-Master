import { useEffect, useRef, useState } from "react";
import {
  Badge,
  Button,
  Card,
  Checkbox,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
  Select,
  Spinner,
  Tab,
  TabList,
} from "@fluentui/react-components";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Link,
  useNavigate,
  useParams,
  useSearchParams,
} from "react-router-dom";
import { useTranslation } from "react-i18next";
import "../i18n/accounts";
import i18n from "../i18n";
import { useAuth } from "../auth/AuthProvider";
import { queryScopeFor } from "../auth/types";
import { ApiError } from "../api/client";
import {
  assignPoolAccount,
  cancelPoolLogin,
  cancelChannelLogin,
  channelKeys,
  completePoolLogin,
  completeChannelLogin,
  createPoolAccount,
  createPoolGroup,
  createChannelAccount,
  createChannelGroup,
  deleteChannelGroup,
  getPoolLoginStatus,
  getChannelLoginStatus,
  authorizePoolDesktop,
  authorizeChannelDesktop,
  listPoolAccounts,
  listPoolAssignments,
  listPoolGroups,
  listOperatorConnectorCapabilities,
  listProjectConnectorCapabilities,
  listChannelPlatforms,
  startPoolLogin,
  startChannelLogin,
  unassignPoolAccount,
  updatePoolAccount,
  updatePoolGroup,
  updateOperatorConnectorCapability,
  updateChannelAccount,
  updateChannelGroup,
  useChannelData,
  type ChannelAccount,
  type ChannelPlatformId,
  type ConnectorAvailability,
  type OperatorConnectorCapability,
  type ProxyInput,
} from "../api/channels";
import { EmptyState, ErrorState, LoadingState } from "../components/AsyncState";
import { RemoteDesktop } from "../components/RemoteDesktop";
import { ProjectAiSettingsPanel } from "./ProjectAiSettingsPanel";
import "../i18n/projectAi";
import "./ChannelAccountsPage.css";

type View = "channels" | "connect" | "settings";
const terminalPhases = new Set(["connected", "cancelled", "expired", "failed"]);
const accountState = (status: ChannelAccount["status"]) =>
  i18n.t(`account.state.${status}`);
const connectorState = (availability: ConnectorAvailability) =>
  i18n.t(`account.connector.${availability}`);
const operatorConnectorState: Record<ConnectorAvailability, string> = {
  unavailable: "未实测可用",
  disabled: "运营方已停用",
  version_mismatch: "连接器版本未验证",
  unsupported_content_type: "内容类型不可用",
  available: "已验证可用",
};

function publicationFormatLabel(format: string) {
  return format === "plain_text_article.v1"
    ? i18n.t("account.connector.article")
    : format;
}

function ConnectorCapabilityRow({
  item,
  queryKey,
  userId,
  operatorId,
}: {
  item: OperatorConnectorCapability;
  queryKey: readonly string[];
  userId: string;
  operatorId: string;
}) {
  const client = useQueryClient();
  const [enabled, setEnabled] = useState(item.enabled);
  const [types, setTypes] = useState(item.content_types);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  useEffect(() => {
    setEnabled(item.enabled);
    setTypes(item.content_types);
  }, [item.enabled, item.content_types, item.revision]);
  const verified = item.verified_content_types;
  const changed =
    enabled !== item.enabled ||
    types.length !== item.content_types.length ||
    types.some((type) => !item.content_types.includes(type));

  async function save() {
    setBusy(true);
    setError("");
    setNotice("");
    try {
      const updated = await updateOperatorConnectorCapability(
        item.platform_id,
        item.placement_slot,
        { expected_revision: item.revision, enabled, content_types: types },
      );
      client.setQueryData<{ items: OperatorConnectorCapability[] }>(
        queryKey,
        (previous) =>
          previous && {
            items: previous.items.map((entry) =>
              entry.platform_id === item.platform_id &&
              entry.placement_slot === item.placement_slot
                ? updated
                : entry,
            ),
          },
      );
      await Promise.all([
        client.invalidateQueries({ queryKey }),
        client.invalidateQueries({
          queryKey: ["project-connector-capabilities", userId, operatorId],
        }),
      ]);
      setNotice("连接器配置已更新；账号登录状态不受此操作影响。");
    } catch (cause) {
      if (cause instanceof ApiError && cause.status === 409) {
        setError("配置已由其他人更新，请检查最新状态后重试。");
        await client.invalidateQueries({ queryKey });
      } else {
        setError(errorText(cause));
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <li>
      <div className="channel-row">
        <div>
          <h3>
            {item.platform_id} · {item.placement_slot}
          </h3>
          <p>
            部署版本：{item.deployed_version ?? "未部署"} · 配置修订{" "}
            {item.revision}
          </p>
        </div>
        <Badge
          color={item.availability === "available" ? "success" : "warning"}
        >
          {operatorConnectorState[item.availability]}
        </Badge>
      </div>
      <p>
        实测发布格式：
        {verified.length
          ? verified.map(publicationFormatLabel).join("、")
          : "尚无真实发布及公开读回验证"}
        。 配置开关仅选择已验证类型，不能创建验证记录。
      </p>
      <Checkbox
        label={`启用 ${item.platform_id} ${item.placement_slot} 连接器`}
        checked={enabled}
        disabled={busy || (!enabled && verified.length === 0)}
        onChange={(_, data) => setEnabled(Boolean(data.checked))}
      />
      {verified.map((type) => (
        <Checkbox
          key={type}
          label={`允许 ${item.platform_id} ${publicationFormatLabel(type)}`}
          checked={types.includes(type)}
          disabled={busy}
          onChange={(_, data) =>
            setTypes((previous) =>
              data.checked
                ? [...previous.filter((value) => value !== type), type]
                : previous.filter((value) => value !== type),
            )
          }
        />
      ))}
      <Button
        disabled={
          busy ||
          !changed ||
          (enabled &&
            (!types.length || types.some((type) => !verified.includes(type))))
        }
        onClick={() => void save()}
      >
        保存连接器配置
      </Button>
      {busy && <Spinner size="tiny" label="正在保存连接器配置" />}
      {error && <ErrorState title="连接器配置未保存" detail={error} />}
      {notice && (
        <MessageBar intent="success">
          <MessageBarBody>{notice}</MessageBarBody>
        </MessageBar>
      )}
    </li>
  );
}

function errorText(error: unknown) {
  return error instanceof Error
    ? error.message
    : i18n.t("account.operationFailed");
}

function RemoteLogin({
  sessionId,
  mode,
  tenantId,
  projectId,
  onDone,
  onClose,
}: {
  sessionId: string;
  mode?: "customer" | "operator";
  tenantId?: string;
  projectId?: string;
  onDone: () => void;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const queryKey =
    mode === "operator"
      ? ["operator-channel-login", sessionId]
      : ["channel-login", tenantId, projectId, sessionId];
  const getStatus = () =>
    mode === "operator"
      ? getPoolLoginStatus(sessionId)
      : getChannelLoginStatus(tenantId!, projectId!, sessionId);
  const authorize = () =>
    mode === "operator"
      ? authorizePoolDesktop(sessionId)
      : authorizeChannelDesktop(tenantId!, projectId!, sessionId);
  const complete = () =>
    mode === "operator"
      ? completePoolLogin(sessionId)
      : completeChannelLogin(tenantId!, projectId!, sessionId);
  const cancelSession = () =>
    mode === "operator"
      ? cancelPoolLogin(sessionId)
      : cancelChannelLogin(tenantId!, projectId!, sessionId);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const completionAttempted = useRef(false);
  const snapshot = useQuery({
    queryKey,
    queryFn: getStatus,
    refetchInterval: (query) =>
      busy ||
      terminalPhases.has(query.state.data?.phase ?? "") ||
      (query.state.error instanceof ApiError &&
        query.state.error.status === 404)
        ? false
        : 2000,
    refetchIntervalInBackground: false,
    retry: false,
    // Account identity must not linger in an inactive cache.
    gcTime: 0,
  });
  const screen = snapshot.data;
  useEffect(() => {
    if (
      !screen?.identity ||
      screen.phase !== "ready_to_complete" ||
      completionAttempted.current
    )
      return;
    completionAttempted.current = true;
    setBusy(true);
    void complete()
      .then(() => onDone())
      .catch((cause) => setError(errorText(cause)))
      .finally(() => setBusy(false));
  }, [
    screen?.identity,
    screen?.phase,
    tenantId,
    projectId,
    sessionId,
    mode,
    onDone,
  ]);

  async function finish() {
    setBusy(true);
    setError("");
    completionAttempted.current = true;
    try {
      await complete();
      onDone();
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      setBusy(false);
    }
  }

  async function cancel() {
    setBusy(true);
    try {
      await cancelSession();
      onClose();
    } catch (cause) {
      if (cause instanceof ApiError && cause.status === 404) {
        // An expired runner session is already gone. Let the user reconnect.
        onClose();
        return;
      }
      setError(errorText(cause));
      setBusy(false);
    }
  }

  const ended = screen && terminalPhases.has(screen.phase);
  return (
    <Card className="channel-login" aria-label={t("account.remote.title")}>
      <div className="channel-row">
        <h2>{t("account.remote.title")}</h2>
        <Button onClick={() => void cancel()} disabled={busy}>
          {t("account.remote.cancel")}
        </Button>
      </div>
      <p>{t("account.remote.help")}</p>
      {snapshot.isPending && (
        <LoadingState label={t("account.remote.launching")} />
      )}
      {snapshot.isError && (
        <ErrorState
          detail={errorText(snapshot.error)}
          onRetry={() => void snapshot.refetch()}
        />
      )}
      {screen && (
        <>
          <div className="channel-row">
            <Badge appearance="tint">
              {t(`remoteDesktop.phase.${screen.phase}`, {
                defaultValue: t("remoteDesktop.phase.pending"),
              })}
            </Badge>
            {snapshot.isFetching && (
              <Spinner size="tiny" label={t("account.remote.checking")} />
            )}
          </div>
          <RemoteDesktop authorize={authorize} active={!ended && !busy} />
          {screen.identity && (
            <p>
              {t("account.remote.identity", {
                name:
                  screen.identity.display_name ??
                  screen.identity.platform_account_id ??
                  t("account.remote.pendingIdentity"),
              })}
            </p>
          )}
          <div className="channel-row">
            {screen.identity && !ended && (
              <span>
                {t(
                  busy ? "account.remote.saving" : "account.remote.identified",
                )}
              </span>
            )}
            {screen.identity && error && !ended && (
              <Button disabled={busy} onClick={() => void finish()}>
                {t("account.remote.retrySave")}
              </Button>
            )}
            {ended && (
              <span>
                {t(
                  screen.phase === "connected"
                    ? "account.remote.connected"
                    : "account.remote.ended",
                )}
              </span>
            )}
          </div>
        </>
      )}
      {error && (
        <ErrorState title={t("account.remote.failed")} detail={error} />
      )}
    </Card>
  );
}

export function ChannelAccountsPage({ view = "channels" }: { view?: View }) {
  const { t } = useTranslation();
  const { tenantId, projectId } = useParams();
  const navigate = useNavigate();
  const [searchParams, setSearchParams] = useSearchParams();
  const aiTab = view === "settings" && searchParams.get("tab") === "ai";
  const { session } = useAuth();
  const client = useQueryClient();
  const { accounts, groups, platforms } = useChannelData(tenantId, projectId);
  const [platform, setPlatform] = useState<ChannelPlatformId>("zhihu");
  const [selectedGroup, setSelectedGroup] = useState("");
  const [groupName, setGroupName] = useState("");
  const [editingGroup, setEditingGroup] = useState<string | null>(null);
  const [deletingGroup, setDeletingGroup] = useState<string | null>(null);
  const [editingName, setEditingName] = useState("");
  const [proxyServer, setProxyServer] = useState("");
  const [proxyUsername, setProxyUsername] = useState("");
  const [proxyPassword, setProxyPassword] = useState("");
  const [editAccountId, setEditAccountId] = useState<string | null>(null);
  const [activeSession, setActiveSession] = useState<string | null>(null);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState(false);
  const activeAccount = accounts.data?.items.find(
    (account) => account.account_id === editAccountId,
  );

  const keyArgs =
    session && tenantId && projectId
      ? queryScopeFor(session, tenantId, projectId)
      : undefined;
  const capabilities = useQuery({
    queryKey: channelKeys.projectCapabilities(
      keyArgs?.userId ?? "",
      keyArgs?.operatorId ?? "",
      keyArgs?.tenantId ?? "",
      keyArgs?.projectId ?? "",
    ),
    queryFn: () => listProjectConnectorCapabilities(tenantId!, projectId!),
    enabled: Boolean(keyArgs) && view === "channels",
    retry: false,
  });
  const invalidate = async () => {
    if (!keyArgs) return;
    await Promise.all([
      client.invalidateQueries({
        queryKey: channelKeys.accounts(
          keyArgs.userId,
          keyArgs.operatorId,
          keyArgs.tenantId,
          keyArgs.projectId!,
        ),
      }),
      client.invalidateQueries({
        queryKey: channelKeys.groups(
          keyArgs.userId,
          keyArgs.operatorId,
          keyArgs.tenantId,
          keyArgs.projectId!,
        ),
      }),
    ]);
  };

  useEffect(() => {
    setActiveSession(null);
    setEditAccountId(null);
  }, [tenantId, projectId]);

  async function run(operation: () => Promise<unknown>, success: string) {
    setBusy(true);
    setError("");
    setNotice("");
    try {
      await operation();
      await invalidate();
      setNotice(success);
      return true;
    } catch (cause) {
      setError(errorText(cause));
      return false;
    } finally {
      setBusy(false);
    }
  }

  function proxyInput(): ProxyInput | undefined {
    const server = proxyServer.trim();
    return server
      ? {
          server,
          ...(proxyUsername ? { username: proxyUsername } : {}),
          ...(proxyPassword ? { password: proxyPassword } : {}),
        }
      : undefined;
  }

  async function connect(accountId?: string) {
    if (!tenantId || !projectId) return;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      let id = accountId;
      if (!id) {
        const created = await createChannelAccount(
          tenantId,
          projectId,
          platform,
          selectedGroup || undefined,
          proxyInput(),
        );
        id = created.account_id;
        setProxyPassword("");
        setProxyUsername("");
        await invalidate();
      }
      const login = await startChannelLogin(tenantId, projectId, id);
      setActiveSession(login.session_id);
      setEditAccountId(null);
    } catch (cause) {
      setProxyPassword("");
      setError(errorText(cause));
    } finally {
      setBusy(false);
    }
  }

  const platformName = (id: ChannelPlatformId) =>
    platforms.data?.items.find((item) => item.id === id)?.label ?? id;
  const groupNameFor = (id: string | null) =>
    groups.data?.items.find((item) => item.group_id === id)?.name ??
    t("account.channels.ungrouped");
  function closeLogin() {
    const completedSession = activeSession;
    setActiveSession(null);
    if (completedSession && tenantId && projectId) {
      client.removeQueries({
        queryKey: ["channel-login", tenantId, projectId, completedSession],
      });
    }
    void invalidate();
  }

  return (
    <div className="channels-page">
      <section className="page-hero">
        <div>
          <p className="eyebrow">
            {t(
              view === "settings"
                ? "account.channels.settingsEyebrow"
                : "account.channels.eyebrow",
            )}
          </p>
          <h1>
            {t(
              view === "settings"
                ? "account.channels.settingsTitle"
                : "account.channels.title",
            )}
          </h1>
          <p>
            {t(
              view === "settings"
                ? "account.channels.settingsDescription"
                : "account.channels.description",
            )}
          </p>
        </div>
        {view !== "connect" && !aiTab && (
          <Button
            onClick={() => navigate("../channels/connect")}
            appearance="primary"
          >
            {t("account.channels.connect")}
          </Button>
        )}
      </section>
      {view === "settings" && (
        <TabList
          aria-label={t("projectAi.tabs")}
          selectedValue={aiTab ? "ai" : "accounts"}
          onTabSelect={(_, data) =>
            setSearchParams((previous) => {
              const next = new URLSearchParams(previous);
              next.set("tab", String(data.value));
              return next;
            })
          }
        >
          <Tab value="accounts">{t("projectAi.accounts")}</Tab>
          <Tab value="ai">{t("projectAi.tab")}</Tab>
        </TabList>
      )}
      {aiTab && tenantId && projectId ? (
        <ProjectAiSettingsPanel
          key={`${tenantId}:${projectId}`}
          tenantId={tenantId}
          projectId={projectId}
        />
      ) : (
        <>
          {view === "settings" && (
            <MessageBar intent="info">
              <MessageBarBody>
                {t("account.channels.settingsNote")}{" "}
                <Link to="../setup">{t("account.channels.viewSetup")}</Link>
              </MessageBarBody>
            </MessageBar>
          )}
          {error && (
            <ErrorState title={t("account.channels.failed")} detail={error} />
          )}
          {notice && (
            <MessageBar intent="success">
              <MessageBarBody>{notice}</MessageBarBody>
            </MessageBar>
          )}
          {activeSession && tenantId && projectId && (
            <RemoteLogin
              key={activeSession}
              tenantId={tenantId}
              projectId={projectId}
              sessionId={activeSession}
              onClose={closeLogin}
              onDone={() => {
                closeLogin();
                setNotice(t("account.channels.connected"));
              }}
            />
          )}
          <div className="channel-layout">
            <section
              className="channel-stack"
              aria-label={t("account.channels.connectionSection")}
            >
              <Card className="channel-card">
                <h2>{t("account.channels.connect")}</h2>
                <p>{t("account.channels.connectionHelp")}</p>
                {platforms.isPending ? (
                  <LoadingState
                    label={t("account.channels.loadingPlatforms")}
                    compact
                  />
                ) : platforms.isError ? (
                  <ErrorState
                    detail={errorText(platforms.error)}
                    onRetry={() => void platforms.refetch()}
                  />
                ) : (
                  <div className="channel-form">
                    <Field label={t("account.channels.platform")}>
                      <Select
                        value={platform}
                        onChange={(event) =>
                          setPlatform(event.target.value as ChannelPlatformId)
                        }
                      >
                        {platforms.data.items.map((item) => (
                          <option
                            key={item.id}
                            value={item.id}
                            disabled={!item.login_supported}
                          >
                            {item.label} ·{" "}
                            {t(
                              item.purpose === "measurement"
                                ? "account.channels.measurement"
                                : "account.channels.publishing",
                            )}
                          </option>
                        ))}
                      </Select>
                    </Field>
                    <Field label={t("account.channels.group")}>
                      <Select
                        value={selectedGroup}
                        onChange={(event) =>
                          setSelectedGroup(event.target.value)
                        }
                      >
                        <option value="">
                          {t("account.channels.ungrouped")}
                        </option>
                        {groups.data?.items.map((group) => (
                          <option key={group.group_id} value={group.group_id}>
                            {group.name}
                          </option>
                        ))}
                      </Select>
                    </Field>
                    <details className="channel-proxy">
                      <summary>{t("account.channels.proxyOptional")}</summary>
                      <p>{t("account.channels.proxyHelp")}</p>
                      <Field label={t("account.channels.proxyAddressType")}>
                        <Input
                          value={proxyServer}
                          onChange={(_, data) => setProxyServer(data.value)}
                          placeholder="socks5://host:port"
                        />
                      </Field>
                      <Field label={t("account.channels.usernameOptional")}>
                        <Input
                          value={proxyUsername}
                          onChange={(_, data) => setProxyUsername(data.value)}
                          autoComplete="off"
                        />
                      </Field>
                      <Field label={t("account.channels.passwordOptional")}>
                        <Input
                          type="password"
                          value={proxyPassword}
                          onChange={(_, data) => setProxyPassword(data.value)}
                          autoComplete="new-password"
                        />
                      </Field>
                    </details>
                    <Button
                      appearance="primary"
                      disabled={
                        busy ||
                        !platforms.data.items.some(
                          (item) =>
                            item.id === platform && item.login_supported,
                        )
                      }
                      onClick={() => void connect()}
                    >
                      {t("account.channels.startLogin")}
                    </Button>
                  </div>
                )}
              </Card>
              <Card className="channel-card">
                <h2>{t("account.channels.group")}</h2>
                <p>{t("account.channels.groupsHelp")}</p>
                <div className="channel-row">
                  <Field label={t("account.channels.newGroup")}>
                    <Input
                      value={groupName}
                      maxLength={80}
                      onChange={(_, data) => setGroupName(data.value)}
                    />
                  </Field>
                  <Button
                    disabled={busy || !groupName.trim()}
                    onClick={() => {
                      if (!tenantId || !projectId) return;
                      void run(
                        () =>
                          createChannelGroup(
                            tenantId,
                            projectId,
                            groupName.trim(),
                          ),
                        t("account.channels.groupCreated"),
                      ).then((saved) => {
                        if (saved) setGroupName("");
                      });
                    }}
                  >
                    {t("account.channels.create")}
                  </Button>
                </div>
                {groups.isPending && (
                  <LoadingState
                    label={t("account.channels.groupsLoading")}
                    compact
                  />
                )}
                {groups.isError && (
                  <ErrorState
                    detail={errorText(groups.error)}
                    onRetry={() => void groups.refetch()}
                  />
                )}
                {groups.data?.items.length === 0 && (
                  <p>{t("account.channels.groupsEmpty")}</p>
                )}
                <ul className="channel-groups">
                  {groups.data?.items.map((group) => (
                    <li key={group.group_id}>
                      {editingGroup === group.group_id ? (
                        <>
                          <Input
                            aria-label={t("account.channels.groupName")}
                            value={editingName}
                            maxLength={80}
                            onChange={(_, data) => setEditingName(data.value)}
                          />
                          <Button
                            disabled={busy || !editingName.trim()}
                            onClick={() => {
                              if (!tenantId || !projectId) return;
                              void run(
                                () =>
                                  updateChannelGroup(
                                    tenantId,
                                    projectId,
                                    group.group_id,
                                    editingName.trim(),
                                  ),
                                t("account.channels.groupUpdated"),
                              ).then((saved) => {
                                if (saved) setEditingGroup(null);
                              });
                            }}
                          >
                            {t("account.channels.save")}
                          </Button>
                          <Button onClick={() => setEditingGroup(null)}>
                            {t("account.channels.cancel")}
                          </Button>
                        </>
                      ) : (
                        <>
                          <span>{group.name}</span>
                          <Button
                            size="small"
                            onClick={() => {
                              setEditingGroup(group.group_id);
                              setEditingName(group.name);
                            }}
                          >
                            {t("account.channels.rename")}
                          </Button>
                          {deletingGroup === group.group_id ? (
                            <>
                              <span>{t("account.channels.deleteConfirm")}</span>
                              <Button
                                size="small"
                                disabled={busy}
                                onClick={() => {
                                  if (!tenantId || !projectId) return;
                                  void run(
                                    () =>
                                      deleteChannelGroup(
                                        tenantId,
                                        projectId,
                                        group.group_id,
                                      ),
                                    t("account.channels.groupDeleted"),
                                  ).then((saved) => {
                                    if (saved) {
                                      if (selectedGroup === group.group_id)
                                        setSelectedGroup("");
                                      setDeletingGroup(null);
                                    }
                                  });
                                }}
                              >
                                {t("account.channels.confirmDelete")}
                              </Button>
                              <Button
                                size="small"
                                onClick={() => setDeletingGroup(null)}
                              >
                                {t("account.channels.cancel")}
                              </Button>
                            </>
                          ) : (
                            <Button
                              size="small"
                              onClick={() => setDeletingGroup(group.group_id)}
                            >
                              {t("account.channels.deleteGroup")}
                            </Button>
                          )}
                        </>
                      )}
                    </li>
                  ))}
                </ul>
              </Card>
            </section>
            <section
              className="channel-stack"
              aria-label={t("account.channels.accountsSection")}
            >
              <Card className="channel-card">
                <div className="channel-row">
                  <h2>{t("account.channels.projectAccounts")}</h2>
                  <Button
                    disabled={accounts.isFetching}
                    onClick={() => void accounts.refetch()}
                  >
                    {t("account.channels.refresh")}
                  </Button>
                </div>
                {accounts.isPending && (
                  <LoadingState label={t("account.channels.accountsLoading")} />
                )}
                {accounts.isError && (
                  <ErrorState
                    detail={errorText(accounts.error)}
                    onRetry={() => void accounts.refetch()}
                  />
                )}
                {accounts.data?.items.length === 0 && (
                  <EmptyState
                    title={t("account.channels.accountsEmpty")}
                    detail={t("account.channels.accountsEmptyDetail")}
                  />
                )}
                <ul className="channel-accounts">
                  {accounts.data?.items.map((account) => (
                    <li key={account.account_id}>
                      <div className="channel-row">
                        <div>
                          <h3>
                            {account.display_name ??
                              t("account.channels.unidentified")}
                          </h3>
                          <p>
                            {platformName(account.platform)} ·{" "}
                            {account.owner_kind === "operator_pool"
                              ? t("account.channels.operatorGroup")
                              : groupNameFor(account.group_id)}
                          </p>
                        </div>
                        <Badge appearance="tint">
                          {account.owner_kind === "operator_pool"
                            ? t("account.channels.operatorShared")
                            : t("account.channels.ownAccount")}
                        </Badge>
                        <Badge
                          color={
                            account.status === "ready" ? "success" : "warning"
                          }
                        >
                          {accountState(account.status)}
                        </Badge>
                      </div>
                      {account.owner_kind === "operator_pool" ? (
                        <p>{t("account.channels.sharedHelp")}</p>
                      ) : (
                        <>
                          <p>
                            {t("account.channels.network", {
                              name: account.proxy_configured
                                ? (account.proxy_server ??
                                  t("account.channels.dedicatedProxy"))
                                : t("account.channels.defaultNetwork"),
                            })}
                          </p>
                          <div className="channel-row">
                            <Button
                              disabled={busy || Boolean(activeSession)}
                              onClick={() => void connect(account.account_id)}
                            >
                              {account.status === "ready"
                                ? t("account.channels.reconnect")
                                : t("account.channels.loginVerify")}
                            </Button>
                            <Button
                              disabled={busy}
                              onClick={() =>
                                setEditAccountId(
                                  editAccountId === account.account_id
                                    ? null
                                    : account.account_id,
                                )
                              }
                            >
                              {t("account.channels.configure")}
                            </Button>
                          </div>
                        </>
                      )}
                      {account.owner_kind !== "operator_pool" &&
                        activeAccount?.account_id === account.account_id && (
                          <div className="channel-account-editor">
                            <Field label={t("account.channels.group")}>
                              <Select
                                value={account.group_id ?? ""}
                                onChange={(event) => {
                                  if (!tenantId || !projectId) return;
                                  void run(
                                    () =>
                                      updateChannelAccount(
                                        tenantId,
                                        projectId,
                                        account.account_id,
                                        {
                                          group_id: event.target.value || null,
                                        },
                                      ),
                                    t("account.channels.accountGroupUpdated"),
                                  );
                                }}
                              >
                                <option value="">
                                  {t("account.channels.ungrouped")}
                                </option>
                                {groups.data?.items.map((group) => (
                                  <option
                                    key={group.group_id}
                                    value={group.group_id}
                                  >
                                    {group.name}
                                  </option>
                                ))}
                              </Select>
                            </Field>
                            <Checkbox
                              label={t("account.channels.allowProject")}
                              checked={account.enabled}
                              disabled={busy}
                              onChange={(_, data) => {
                                if (!tenantId || !projectId) return;
                                void run(
                                  () =>
                                    updateChannelAccount(
                                      tenantId,
                                      projectId,
                                      account.account_id,
                                      { enabled: Boolean(data.checked) },
                                    ),
                                  t("account.channels.usageUpdated"),
                                );
                              }}
                            />
                            <details className="channel-proxy">
                              <summary>
                                {t("account.channels.changeProxy")}
                              </summary>
                              <p>{t("account.channels.changeProxyHelp")}</p>
                              <Field label={t("account.channels.proxyAddress")}>
                                <Input
                                  value={proxyServer}
                                  onChange={(_, data) =>
                                    setProxyServer(data.value)
                                  }
                                />
                              </Field>
                              <Field label={t("account.channels.username")}>
                                <Input
                                  value={proxyUsername}
                                  onChange={(_, data) =>
                                    setProxyUsername(data.value)
                                  }
                                  autoComplete="off"
                                />
                              </Field>
                              <Field label={t("account.channels.password")}>
                                <Input
                                  type="password"
                                  value={proxyPassword}
                                  onChange={(_, data) =>
                                    setProxyPassword(data.value)
                                  }
                                  autoComplete="new-password"
                                />
                              </Field>
                              <Button
                                disabled={busy || !proxyServer.trim()}
                                onClick={() => {
                                  if (!tenantId || !projectId) return;
                                  void run(
                                    () =>
                                      updateChannelAccount(
                                        tenantId,
                                        projectId,
                                        account.account_id,
                                        { proxy: proxyInput() },
                                      ),
                                    t("account.channels.proxyUpdated"),
                                  ).finally(() => {
                                    setProxyPassword("");
                                    setProxyUsername("");
                                    setProxyServer("");
                                  });
                                }}
                              >
                                {t("account.channels.saveProxy")}
                              </Button>
                              {account.proxy_configured && (
                                <Button
                                  disabled={busy}
                                  onClick={() => {
                                    if (!tenantId || !projectId) return;
                                    void run(
                                      () =>
                                        updateChannelAccount(
                                          tenantId,
                                          projectId,
                                          account.account_id,
                                          { proxy: null },
                                        ),
                                      t("account.channels.defaultRestored"),
                                    );
                                  }}
                                >
                                  {t("account.channels.removeProxy")}
                                </Button>
                              )}
                            </details>
                          </div>
                        )}
                    </li>
                  ))}
                </ul>
              </Card>
            </section>
          </div>
          {view === "channels" && (
            <section aria-label={t("account.connector.section")}>
              <Card className="channel-card">
                <h2>{t("account.connector.section")}</h2>
                <p>{t("account.connector.description")}</p>
                {capabilities.isPending && (
                  <LoadingState label={t("account.connector.loading")} />
                )}
                {capabilities.isError &&
                  (capabilities.error instanceof ApiError &&
                  capabilities.error.status === 403 ? (
                    <ErrorState
                      title={t("account.channels.permissionDenied")}
                      detail={t("account.connector.forbidden")}
                    />
                  ) : (
                    <ErrorState
                      detail={errorText(capabilities.error)}
                      onRetry={() => void capabilities.refetch()}
                    />
                  ))}
                {capabilities.data?.items.length === 0 && (
                  <EmptyState
                    title={t("account.connector.empty")}
                    detail={t("account.connector.emptyDetail")}
                  />
                )}
                <ul className="channel-accounts">
                  {capabilities.data?.items.map((item) => (
                    <li key={`${item.platform_id}:${item.placement_slot}`}>
                      <div className="channel-row">
                        <h3>
                          {item.platform_id} · {item.placement_slot}
                        </h3>
                        <Badge
                          color={
                            item.availability === "available"
                              ? "success"
                              : "warning"
                          }
                        >
                          {connectorState(item.availability)}
                        </Badge>
                      </div>
                      <p>
                        {item.availability === "available"
                          ? t("account.connector.formats", {
                              formats:
                                item.content_types
                                  .map(publicationFormatLabel)
                                  .join(t("account.connector.separator")) ||
                                t("account.connector.none"),
                            })
                          : t("account.connector.unavailableDetail")}
                      </p>
                    </li>
                  ))}
                </ul>
              </Card>
            </section>
          )}
          <MessageBar intent="info">
            <MessageBarBody>
              {t("account.channels.webLoginNote")}
            </MessageBarBody>
          </MessageBar>
        </>
      )}
    </div>
  );
}

export function OperatorAccountsPage() {
  const { session } = useAuth();
  const { t } = useTranslation();
  const client = useQueryClient();
  const allowed = Boolean(
    session?.memberships.some(
      (membership) =>
        membership.role === "oem_admin" || membership.role === "resource_admin",
    ),
  );
  const ownerKey = [session?.user.id, session?.operator.id];
  const accountsKey = ["operator-channel-accounts", ...ownerKey];
  const groupsKey = ["operator-channel-groups", ...ownerKey];
  const capabilitiesKey = channelKeys.operatorCapabilities(
    session?.user.id ?? "",
    session?.operator.id ?? "",
  );
  const capabilities = useQuery({
    queryKey: capabilitiesKey,
    queryFn: listOperatorConnectorCapabilities,
    enabled: allowed,
    retry: false,
  });
  const accounts = useQuery({
    queryKey: accountsKey,
    queryFn: listPoolAccounts,
    enabled: allowed,
  });
  const groups = useQuery({
    queryKey: groupsKey,
    queryFn: listPoolGroups,
    enabled: allowed,
  });
  const platforms = useQuery({
    queryKey: channelKeys.platforms,
    queryFn: listChannelPlatforms,
    enabled: allowed,
  });
  const [platform, setPlatform] = useState<ChannelPlatformId>("zhihu");
  const [groupId, setGroupId] = useState("");
  const [groupName, setGroupName] = useState("");
  const [renameId, setRenameId] = useState<string | null>(null);
  const [renameName, setRenameName] = useState("");
  const [proxyServer, setProxyServer] = useState("");
  const [proxyUsername, setProxyUsername] = useState("");
  const [proxyPassword, setProxyPassword] = useState("");
  const [editProxyServer, setEditProxyServer] = useState("");
  const [editProxyUsername, setEditProxyUsername] = useState("");
  const [editProxyPassword, setEditProxyPassword] = useState("");
  const [selectedAccount, setSelectedAccount] = useState<string | null>(null);
  const [targetTenant, setTargetTenant] = useState("");
  const [targetProject, setTargetProject] = useState("");
  const [activeSession, setActiveSession] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const assignmentsKey = [
    "operator-channel-assignments",
    ...ownerKey,
    selectedAccount,
  ];
  const assignments = useQuery({
    queryKey: assignmentsKey,
    queryFn: () => listPoolAssignments(selectedAccount!),
    enabled: allowed && Boolean(selectedAccount),
  });
  const refresh = async () =>
    Promise.all([
      client.invalidateQueries({ queryKey: accountsKey }),
      client.invalidateQueries({ queryKey: groupsKey }),
      client.invalidateQueries({ queryKey: assignmentsKey }),
    ]);

  async function run(operation: () => Promise<unknown>, success: string) {
    setBusy(true);
    setError("");
    setNotice("");
    try {
      await operation();
      await refresh();
      setNotice(success);
      return true;
    } catch (cause) {
      setError(errorText(cause));
      return false;
    } finally {
      setBusy(false);
      setProxyPassword("");
    }
  }

  async function connect(accountId?: string) {
    setBusy(true);
    setError("");
    try {
      let id = accountId;
      if (!id) {
        const server = proxyServer.trim();
        const proxy: ProxyInput | undefined = server
          ? {
              server,
              ...(proxyUsername ? { username: proxyUsername } : {}),
              ...(proxyPassword ? { password: proxyPassword } : {}),
            }
          : undefined;
        const account = await createPoolAccount(
          platform,
          groupId || undefined,
          proxy,
        );
        id = account.account_id;
        await refresh();
      }
      const login = await startPoolLogin(id);
      setActiveSession(login.session_id);
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      setProxyPassword("");
      setProxyUsername("");
      setBusy(false);
    }
  }

  function closeLogin() {
    if (activeSession)
      client.removeQueries({
        queryKey: ["operator-channel-login", activeSession],
      });
    setActiveSession(null);
    void refresh();
  }

  if (!allowed)
    return (
      <div className="channels-page">
        <ErrorState
          title="权限不足"
          detail="只有总部资源管理员可以管理共享账号资源池。"
        />
      </div>
    );

  return (
    <div className="channels-page">
      <section className="page-hero">
        <div>
          <p className="eyebrow">总部管理 · 共享账号池</p>
          <h1>账号资源池</h1>
          <p>
            总部连接并维护共享账号，再明确分配给客户项目。客户不能重新登录或修改总部账号。
          </p>
          {session?.memberships.some(
            (membership) => membership.role === "oem_admin",
          ) && <Link to="/ops/appearance">{t("appearance.title")}</Link>}
        </div>
      </section>
      {error && <ErrorState title="操作未完成" detail={error} />}
      {notice && (
        <MessageBar intent="success">
          <MessageBarBody>{notice}</MessageBarBody>
        </MessageBar>
      )}
      {activeSession && (
        <RemoteLogin
          key={activeSession}
          mode="operator"
          sessionId={activeSession}
          onClose={closeLogin}
          onDone={() => {
            closeLogin();
            setNotice("总部账号身份已验证并保存。");
          }}
        />
      )}
      <div className="channel-layout">
        <section className="channel-stack" aria-label="总部账号接入">
          <Card className="channel-card">
            <h2>接入总部账号</h2>
            <p>总部资源组与客户项目组相互独立；创建后完成远程登录。</p>
            {platforms.isPending ? (
              <LoadingState label="正在加载平台" compact />
            ) : platforms.isError ? (
              <ErrorState
                detail={errorText(platforms.error)}
                onRetry={() => void platforms.refetch()}
              />
            ) : (
              <div className="channel-form">
                <Field label="平台">
                  <Select
                    value={platform}
                    onChange={(event) =>
                      setPlatform(event.target.value as ChannelPlatformId)
                    }
                  >
                    {platforms.data.items.map((item) => (
                      <option
                        key={item.id}
                        value={item.id}
                        disabled={!item.login_supported}
                      >
                        {item.label} ·{" "}
                        {item.purpose === "measurement"
                          ? "网页测量"
                          : "自有账号发布"}
                      </option>
                    ))}
                  </Select>
                </Field>
                <Field label="总部资源组">
                  <Select
                    value={groupId}
                    onChange={(event) => setGroupId(event.target.value)}
                  >
                    <option value="">未分组</option>
                    {groups.data?.items.map((group) => (
                      <option key={group.group_id} value={group.group_id}>
                        {group.name}
                      </option>
                    ))}
                  </Select>
                </Field>
                <details className="channel-proxy">
                  <summary>可选：总部账号代理</summary>
                  <Field label="代理地址">
                    <Input
                      value={proxyServer}
                      onChange={(_, data) => setProxyServer(data.value)}
                    />
                  </Field>
                  <Field label="用户名">
                    <Input
                      value={proxyUsername}
                      onChange={(_, data) => setProxyUsername(data.value)}
                      autoComplete="off"
                    />
                  </Field>
                  <Field label="密码">
                    <Input
                      type="password"
                      value={proxyPassword}
                      onChange={(_, data) => setProxyPassword(data.value)}
                      autoComplete="new-password"
                    />
                  </Field>
                </details>
                <Button
                  appearance="primary"
                  disabled={busy || Boolean(activeSession)}
                  onClick={() => void connect()}
                >
                  创建并登录总部账号
                </Button>
              </div>
            )}
          </Card>
          <Card className="channel-card">
            <h2>总部资源组</h2>
            <div className="channel-row">
              <Field label="新建总部资源组">
                <Input
                  value={groupName}
                  maxLength={80}
                  onChange={(_, data) => setGroupName(data.value)}
                />
              </Field>
              <Button
                disabled={busy || !groupName.trim()}
                onClick={() =>
                  void run(
                    () => createPoolGroup(groupName.trim()),
                    "总部资源组已创建。",
                  ).then((saved) => {
                    if (saved) setGroupName("");
                  })
                }
              >
                创建
              </Button>
            </div>
            {groups.isPending && (
              <LoadingState label="正在加载资源组" compact />
            )}
            {groups.isError && (
              <ErrorState
                detail={errorText(groups.error)}
                onRetry={() => void groups.refetch()}
              />
            )}
            <ul className="channel-groups">
              {groups.data?.items.map((group) => (
                <li key={group.group_id}>
                  {renameId === group.group_id ? (
                    <>
                      <Input
                        aria-label="总部资源组名称"
                        value={renameName}
                        onChange={(_, data) => setRenameName(data.value)}
                      />
                      <Button
                        disabled={busy || !renameName.trim()}
                        onClick={() =>
                          void run(
                            () =>
                              updatePoolGroup(
                                group.group_id,
                                renameName.trim(),
                              ),
                            "总部资源组已更新。",
                          ).then((saved) => {
                            if (saved) setRenameId(null);
                          })
                        }
                      >
                        保存
                      </Button>
                    </>
                  ) : (
                    <>
                      <span>{group.name}</span>
                      <Button
                        onClick={() => {
                          setRenameId(group.group_id);
                          setRenameName(group.name);
                        }}
                      >
                        重命名
                      </Button>
                    </>
                  )}
                </li>
              ))}
            </ul>
          </Card>
        </section>
        <section className="channel-stack" aria-label="总部共享账号">
          <Card className="channel-card">
            <h2>总部账号</h2>
            {accounts.isPending && <LoadingState label="正在加载共享账号" />}
            {accounts.isError && (
              <ErrorState
                detail={errorText(accounts.error)}
                onRetry={() => void accounts.refetch()}
              />
            )}
            {accounts.data?.items.length === 0 && (
              <EmptyState
                title="尚无总部账号"
                detail="创建账号并完成登录后，可按项目明确分配共享资源。"
              />
            )}
            <ul className="channel-accounts">
              {accounts.data?.items.map((account) => (
                <li key={account.account_id}>
                  <div className="channel-row">
                    <div>
                      <h3>{account.display_name ?? "未识别账号"}</h3>
                      <p>
                        {platforms.data?.items.find(
                          (item) => item.id === account.platform,
                        )?.label ?? account.platform}{" "}
                        ·{" "}
                        {groups.data?.items.find(
                          (group) => group.group_id === account.group_id,
                        )?.name ?? "未分组"}
                      </p>
                    </div>
                    <Badge
                      color={account.status === "ready" ? "success" : "warning"}
                    >
                      {accountState(account.status)}
                    </Badge>
                  </div>
                  <div className="channel-row">
                    <Button
                      disabled={busy || Boolean(activeSession)}
                      onClick={() => void connect(account.account_id)}
                    >
                      重新登录
                    </Button>
                    <Button
                      onClick={() =>
                        setSelectedAccount(
                          selectedAccount === account.account_id
                            ? null
                            : account.account_id,
                        )
                      }
                    >
                      项目分配
                    </Button>
                    <Checkbox
                      label="启用"
                      checked={account.enabled}
                      disabled={busy}
                      onChange={(_, data) =>
                        void run(
                          () =>
                            updatePoolAccount(account.account_id, {
                              enabled: Boolean(data.checked),
                            }),
                          "总部账号状态已更新。",
                        )
                      }
                    />
                  </div>
                  {selectedAccount === account.account_id && (
                    <div className="channel-account-editor">
                      <Field label="共享账号资源组">
                        <Select
                          value={account.group_id ?? ""}
                          disabled={busy}
                          onChange={(event) =>
                            void run(
                              () =>
                                updatePoolAccount(account.account_id, {
                                  group_id: event.target.value || null,
                                }),
                              "总部账号资源组已更新。",
                            )
                          }
                        >
                          <option value="">未分组</option>
                          {groups.data?.items.map((group) => (
                            <option key={group.group_id} value={group.group_id}>
                              {group.name}
                            </option>
                          ))}
                        </Select>
                      </Field>
                      <details className="channel-proxy">
                        <summary>管理总部账号代理</summary>
                        <p>
                          当前：
                          {account.proxy_configured
                            ? (account.proxy_server ?? "已配置")
                            : "默认出口"}
                          。更改后不回显凭据。
                        </p>
                        <Field label="新代理地址">
                          <Input
                            value={editProxyServer}
                            onChange={(_, data) =>
                              setEditProxyServer(data.value)
                            }
                          />
                        </Field>
                        <Field label="新代理用户名">
                          <Input
                            value={editProxyUsername}
                            autoComplete="off"
                            onChange={(_, data) =>
                              setEditProxyUsername(data.value)
                            }
                          />
                        </Field>
                        <Field label="新代理密码">
                          <Input
                            type="password"
                            value={editProxyPassword}
                            autoComplete="new-password"
                            onChange={(_, data) =>
                              setEditProxyPassword(data.value)
                            }
                          />
                        </Field>
                        <Button
                          disabled={busy || !editProxyServer.trim()}
                          onClick={() =>
                            void run(
                              () =>
                                updatePoolAccount(account.account_id, {
                                  proxy: {
                                    server: editProxyServer.trim(),
                                    ...(editProxyUsername
                                      ? { username: editProxyUsername }
                                      : {}),
                                    ...(editProxyPassword
                                      ? { password: editProxyPassword }
                                      : {}),
                                  },
                                }),
                              "总部账号代理已更新。",
                            ).finally(() => {
                              setEditProxyServer("");
                              setEditProxyUsername("");
                              setEditProxyPassword("");
                            })
                          }
                        >
                          保存代理
                        </Button>
                        {account.proxy_configured && (
                          <Button
                            disabled={busy}
                            onClick={() =>
                              void run(
                                () =>
                                  updatePoolAccount(account.account_id, {
                                    proxy: null,
                                  }),
                                "总部账号已恢复默认出口。",
                              )
                            }
                          >
                            移除代理
                          </Button>
                        )}
                      </details>
                      <h4>分配给客户项目</h4>
                      <p>
                        填写项目路径中的租户与项目 ID；只有明确分配的项目可见。
                      </p>
                      <Field label="目标租户 ID">
                        <Input
                          value={targetTenant}
                          onChange={(_, data) => setTargetTenant(data.value)}
                        />
                      </Field>
                      <Field label="目标项目 ID">
                        <Input
                          value={targetProject}
                          onChange={(_, data) => setTargetProject(data.value)}
                        />
                      </Field>
                      <Button
                        disabled={
                          busy || !targetTenant.trim() || !targetProject.trim()
                        }
                        onClick={() =>
                          void run(
                            () =>
                              assignPoolAccount(account.account_id, {
                                tenant_id: targetTenant.trim(),
                                project_id: targetProject.trim(),
                              }),
                            "共享账号已分配给项目。",
                          )
                        }
                      >
                        分配项目
                      </Button>
                      {assignments.isPending && (
                        <LoadingState label="正在加载已分配项目" compact />
                      )}
                      {assignments.isError && (
                        <ErrorState
                          detail={errorText(assignments.error)}
                          onRetry={() => void assignments.refetch()}
                        />
                      )}
                      <ul className="channel-groups">
                        {assignments.data?.items.map((target) => (
                          <li key={`${target.tenant_id}:${target.project_id}`}>
                            <span>
                              {target.tenant_id} / {target.project_id}
                            </span>
                            <Button
                              disabled={busy}
                              onClick={() =>
                                void run(
                                  () =>
                                    unassignPoolAccount(
                                      account.account_id,
                                      target,
                                    ),
                                  "项目分配已撤销。",
                                )
                              }
                            >
                              撤销分配
                            </Button>
                          </li>
                        ))}
                      </ul>
                    </div>
                  )}
                </li>
              ))}
            </ul>
          </Card>
        </section>
      </div>
      <section aria-label="运营连接器能力">
        <Card className="channel-card">
          <h2>发布连接器能力</h2>
          <p>
            登录和账号启用不证明发布能力。仅真实发布及公开读回核验过、且与当前部署版本一致的内容类型可由运营方启用。
          </p>
          {capabilities.isPending && (
            <LoadingState label="正在加载发布连接器能力" />
          )}
          {capabilities.isError &&
            (capabilities.error instanceof ApiError &&
            capabilities.error.status === 403 ? (
              <ErrorState title="权限不足" detail="无权管理运营连接器能力。" />
            ) : (
              <ErrorState
                detail={errorText(capabilities.error)}
                onRetry={() => void capabilities.refetch()}
              />
            ))}
          {capabilities.data?.items.length === 0 && (
            <EmptyState
              title="尚无发布连接器"
              detail="尚未发现平台连接器；账号登录无法替代发布与公开读回验证。"
            />
          )}
          <ul className="channel-accounts">
            {capabilities.data?.items.map((item) => (
              <ConnectorCapabilityRow
                key={`${item.platform_id}:${item.placement_slot}`}
                item={item}
                queryKey={capabilitiesKey}
                userId={session?.user.id ?? ""}
                operatorId={session?.operator.id ?? ""}
              />
            ))}
          </ul>
        </Card>
      </section>
    </div>
  );
}
