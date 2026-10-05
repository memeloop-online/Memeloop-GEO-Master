import { useEffect, useRef, useState, type MouseEvent } from "react";
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
} from "@fluentui/react-components";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useParams } from "react-router-dom";
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
  getPoolLoginSnapshot,
  getChannelLoginSnapshot,
  listPoolAccounts,
  listPoolAssignments,
  listPoolGroups,
  listOperatorConnectorCapabilities,
  listProjectConnectorCapabilities,
  listChannelPlatforms,
  sendPoolLoginAction,
  sendChannelLoginAction,
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
  type LoginAction,
  type LoginSnapshot,
  type OperatorConnectorCapability,
  type ProxyInput,
} from "../api/channels";
import { EmptyState, ErrorState, LoadingState } from "../components/AsyncState";
import "./ChannelAccountsPage.css";

type View = "channels" | "connect" | "settings";
const terminalPhases = new Set(["connected", "cancelled", "expired", "failed"]);
const accountState: Record<ChannelAccount["status"], string> = {
  needs_login: "需要登录",
  unverified: "等待身份验证",
  ready: "已连接",
  disabled: "已停用",
  expired: "登录已失效",
};
const connectorState: Record<ConnectorAvailability, string> = {
  unavailable: "未实测可用",
  disabled: "运营方已停用",
  version_mismatch: "连接器版本未验证",
  unsupported_content_type: "内容类型不可用",
  available: "已验证可用",
};

function publicationFormatLabel(format: string) {
  return format === "plain_text_article.v1"
    ? "纯文本文章（标题与正文）"
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
          {connectorState[item.availability]}
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
  return error instanceof Error ? error.message : "操作失败，请重试。";
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
  const client = useQueryClient();
  const queryKey =
    mode === "operator"
      ? ["operator-channel-login", sessionId]
      : ["channel-login", tenantId, projectId, sessionId];
  const getSnapshot = () =>
    mode === "operator"
      ? getPoolLoginSnapshot(sessionId)
      : getChannelLoginSnapshot(tenantId!, projectId!, sessionId);
  const sendAction = (value: LoginAction) =>
    mode === "operator"
      ? sendPoolLoginAction(sessionId, value)
      : sendChannelLoginAction(tenantId!, projectId!, sessionId, value);
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
  const [text, setText] = useState("");
  const [key, setKey] = useState("Enter");
  const [clickX, setClickX] = useState("");
  const [clickY, setClickY] = useState("");
  const completionAttempted = useRef(false);
  const snapshot = useQuery({
    queryKey,
    queryFn: getSnapshot,
    refetchInterval: (query) =>
      busy ||
      terminalPhases.has(query.state.data?.phase ?? "") ||
      (query.state.error instanceof ApiError &&
        query.state.error.status === 404)
        ? false
        : 2000,
    refetchIntervalInBackground: false,
    retry: false,
    // Account screenshots and identity must not linger in an inactive cache.
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

  async function action(value: LoginAction) {
    setBusy(true);
    setError("");
    if (value.kind === "type") setText("");
    try {
      const next = await sendAction(value);
      client.setQueryData<LoginSnapshot>(queryKey, next);
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      setBusy(false);
    }
  }

  function clickScreen(event: MouseEvent<HTMLImageElement>) {
    if (!screen || busy || terminalPhases.has(screen.phase)) return;
    const rect = event.currentTarget.getBoundingClientRect();
    if (!rect.width || !rect.height) return;
    const x = Math.max(
      0,
      Math.min(
        screen.width - 1,
        Math.floor(((event.clientX - rect.left) / rect.width) * screen.width),
      ),
    );
    const y = Math.max(
      0,
      Math.min(
        screen.height - 1,
        Math.floor(((event.clientY - rect.top) / rect.height) * screen.height),
      ),
    );
    void action({ kind: "click", x, y });
  }

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
    <Card className="channel-login" aria-label="远程登录">
      <div className="channel-row">
        <h2>远程登录</h2>
        <Button onClick={() => void cancel()} disabled={busy}>
          取消并关闭
        </Button>
      </div>
      <p>在下方远程页面完成平台登录。识别到账号身份后会自动保存连接。</p>
      {snapshot.isPending && <LoadingState label="正在启动远程浏览器" />}
      {snapshot.isError && (
        <ErrorState
          detail={errorText(snapshot.error)}
          onRetry={() => void snapshot.refetch()}
        />
      )}
      {screen && (
        <>
          <div className="channel-row">
            <Badge appearance="tint">{screen.phase}</Badge>
            {snapshot.isFetching && (
              <Spinner size="tiny" label="正在更新截图" />
            )}
          </div>
          {screen.screenshot_base64 && (
            <div
              className="channel-screen"
              role="group"
              aria-label="远程浏览器画面"
            >
              <img
                src={`data:image/png;base64,${screen.screenshot_base64}`}
                width={screen.width}
                height={screen.height}
                alt="远程登录页面截图；可点击画面，或使用下方键盘操作"
                onClick={clickScreen}
                draggable={false}
              />
            </div>
          )}
          {!ended && (
            <div className="channel-controls">
              <Field label="点击横坐标">
                <Input
                  type="number"
                  min={0}
                  max={screen.width - 1}
                  value={clickX}
                  onChange={(_, data) => setClickX(data.value)}
                />
              </Field>
              <Field label="点击纵坐标">
                <Input
                  type="number"
                  min={0}
                  max={screen.height - 1}
                  value={clickY}
                  onChange={(_, data) => setClickY(data.value)}
                />
              </Field>
              <Button
                disabled={
                  busy ||
                  clickX === "" ||
                  clickY === "" ||
                  Number(clickX) < 0 ||
                  Number(clickX) >= screen.width ||
                  Number(clickY) < 0 ||
                  Number(clickY) >= screen.height
                }
                onClick={() =>
                  void action({
                    kind: "click",
                    x: Number(clickX),
                    y: Number(clickY),
                  })
                }
              >
                点击坐标
              </Button>
              <Field
                label="向当前焦点输入文字"
                hint="本机输入默认掩码，仅发送到当前远程会话并立即清空；不要输入到聊天中。"
              >
                <Input
                  type="password"
                  value={text}
                  onChange={(_, data) => setText(data.value)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter" && text && !busy) {
                      event.preventDefault();
                      void action({ kind: "type", text });
                    }
                  }}
                  autoComplete="off"
                />
              </Field>
              <Button
                disabled={!text || busy}
                onClick={() => void action({ kind: "type", text })}
              >
                发送文字
              </Button>
              <Field label="按键">
                <Select
                  value={key}
                  onChange={(event) => setKey(event.target.value)}
                >
                  {[
                    "Enter",
                    "Tab",
                    "Escape",
                    "Backspace",
                    "ArrowUp",
                    "ArrowDown",
                  ].map((option) => (
                    <option key={option} value={option}>
                      {option}
                    </option>
                  ))}
                </Select>
              </Field>
              <Button
                disabled={busy}
                onClick={() => void action({ kind: "key", key })}
              >
                发送按键
              </Button>
              <Button
                disabled={busy}
                onClick={() => void action({ kind: "scroll", delta_y: 550 })}
              >
                向下滚动
              </Button>
              <Button
                disabled={busy}
                onClick={() => void action({ kind: "scroll", delta_y: -550 })}
              >
                向上滚动
              </Button>
            </div>
          )}
          {screen.identity && (
            <p>
              检测到账号：
              {screen.identity.display_name ??
                screen.identity.platform_account_id ??
                "待核验"}
            </p>
          )}
          <div className="channel-row">
            {screen.identity && !ended && (
              <span>{busy ? "正在验证并保存连接…" : "身份已识别。"}</span>
            )}
            {screen.identity && error && !ended && (
              <Button disabled={busy} onClick={() => void finish()}>
                重试保存连接
              </Button>
            )}
            {ended && <span>登录会话已结束。请关闭后重新连接。</span>}
          </div>
        </>
      )}
      {error && <ErrorState title="远程登录操作未完成" detail={error} />}
    </Card>
  );
}

export function ChannelAccountsPage({ view = "channels" }: { view?: View }) {
  const { tenantId, projectId } = useParams();
  const navigate = useNavigate();
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
    groups.data?.items.find((item) => item.group_id === id)?.name ?? "未分组";
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
            {view === "settings" ? "P16 · 项目设置" : "P10—P11 · 账号与资源"}
          </p>
          <h1>{view === "settings" ? "项目设置与渠道账号" : "渠道账号"}</h1>
          <p>
            连接自有发布账号与网页测量账号，设置分组及可选网络出口。连接状态来自服务端验证。
          </p>
        </div>
        {view !== "connect" && (
          <Button
            onClick={() => navigate("../channels/connect")}
            appearance="primary"
          >
            接入账号
          </Button>
        )}
      </section>
      {view === "settings" && (
        <MessageBar intent="info">
          <MessageBarBody>
            此处管理项目渠道连接；其他项目基本配置仍在项目启动向导中。{" "}
            <Link to="../setup">查看项目配置</Link>
          </MessageBarBody>
        </MessageBar>
      )}
      {error && <ErrorState title="操作未完成" detail={error} />}
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
            setNotice("已验证账号身份并保存连接。");
          }}
        />
      )}
      <div className="channel-layout">
        <section className="channel-stack" aria-label="账号连接">
          <Card className="channel-card">
            <h2>接入账号</h2>
            <p>
              选择平台后启动远程登录。会话凭据保存在服务端，不会写入浏览器存储。
            </p>
            {platforms.isPending ? (
              <LoadingState label="正在加载支持的平台" compact />
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
                <Field label="资源组">
                  <Select
                    value={selectedGroup}
                    onChange={(event) => setSelectedGroup(event.target.value)}
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
                  <summary>可选：指定网络代理</summary>
                  <p>指定代理不可用时不会静默直连。凭据不会回显。</p>
                  <Field label="代理地址（HTTP CONNECT / SOCKS5）">
                    <Input
                      value={proxyServer}
                      onChange={(_, data) => setProxyServer(data.value)}
                      placeholder="socks5://host:port"
                    />
                  </Field>
                  <Field label="用户名（可选）">
                    <Input
                      value={proxyUsername}
                      onChange={(_, data) => setProxyUsername(data.value)}
                      autoComplete="off"
                    />
                  </Field>
                  <Field label="密码（可选）">
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
                      (item) => item.id === platform && item.login_supported,
                    )
                  }
                  onClick={() => void connect()}
                >
                  启动远程登录
                </Button>
              </div>
            )}
          </Card>
          <Card className="channel-card">
            <h2>资源组</h2>
            <p>将多个账号归在同一组；分组不会改变登录流程或自动重复发布。</p>
            <div className="channel-row">
              <Field label="新建资源组">
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
                      createChannelGroup(tenantId, projectId, groupName.trim()),
                    "资源组已创建。",
                  ).then((saved) => {
                    if (saved) setGroupName("");
                  });
                }}
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
            {groups.data?.items.length === 0 && (
              <p>尚无资源组，账号可先保持未分组。</p>
            )}
            <ul className="channel-groups">
              {groups.data?.items.map((group) => (
                <li key={group.group_id}>
                  {editingGroup === group.group_id ? (
                    <>
                      <Input
                        aria-label="资源组名称"
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
                            "资源组已更新。",
                          ).then((saved) => {
                            if (saved) setEditingGroup(null);
                          });
                        }}
                      >
                        保存
                      </Button>
                      <Button onClick={() => setEditingGroup(null)}>
                        取消
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
                        重命名
                      </Button>
                      {deletingGroup === group.group_id ? (
                        <>
                          <span>确定删除此组？组内账号需先移出。</span>
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
                                "资源组已删除。",
                              ).then((saved) => {
                                if (saved) {
                                  if (selectedGroup === group.group_id)
                                    setSelectedGroup("");
                                  setDeletingGroup(null);
                                }
                              });
                            }}
                          >
                            确认删除
                          </Button>
                          <Button
                            size="small"
                            onClick={() => setDeletingGroup(null)}
                          >
                            取消
                          </Button>
                        </>
                      ) : (
                        <Button
                          size="small"
                          onClick={() => setDeletingGroup(group.group_id)}
                        >
                          删除组
                        </Button>
                      )}
                    </>
                  )}
                </li>
              ))}
            </ul>
          </Card>
        </section>
        <section className="channel-stack" aria-label="已接入账号">
          <Card className="channel-card">
            <div className="channel-row">
              <h2>项目账号</h2>
              <Button
                disabled={accounts.isFetching}
                onClick={() => void accounts.refetch()}
              >
                刷新状态
              </Button>
            </div>
            {accounts.isPending && <LoadingState label="正在加载账号" />}
            {accounts.isError && (
              <ErrorState
                detail={errorText(accounts.error)}
                onRetry={() => void accounts.refetch()}
              />
            )}
            {accounts.data?.items.length === 0 && (
              <EmptyState
                title="尚未连接账号"
                detail="选择平台并完成远程登录后，经过身份验证的账号会出现在这里。"
              />
            )}
            <ul className="channel-accounts">
              {accounts.data?.items.map((account) => (
                <li key={account.account_id}>
                  <div className="channel-row">
                    <div>
                      <h3>{account.display_name ?? "未识别账号"}</h3>
                      <p>
                        {platformName(account.platform)} ·{" "}
                        {account.owner_kind === "operator_pool"
                          ? "总部资源池"
                          : groupNameFor(account.group_id)}
                      </p>
                    </div>
                    <Badge appearance="tint">
                      {account.owner_kind === "operator_pool"
                        ? "总部共享"
                        : "自有账号"}
                    </Badge>
                    <Badge
                      color={account.status === "ready" ? "success" : "warning"}
                    >
                      {accountState[account.status]}
                    </Badge>
                  </div>
                  {account.owner_kind === "operator_pool" ? (
                    <p>
                      由总部管理登录和网络出口；本项目只使用已分配的共享资源。
                    </p>
                  ) : (
                    <>
                      <p>
                        网络：
                        {account.proxy_configured
                          ? (account.proxy_server ?? "已配置专用代理")
                          : "默认出口"}
                      </p>
                      <div className="channel-row">
                        <Button
                          disabled={busy || Boolean(activeSession)}
                          onClick={() => void connect(account.account_id)}
                        >
                          {account.status === "ready"
                            ? "重新连接"
                            : "登录并验证"}
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
                          配置
                        </Button>
                      </div>
                    </>
                  )}
                  {account.owner_kind !== "operator_pool" &&
                    activeAccount?.account_id === account.account_id && (
                      <div className="channel-account-editor">
                        <Field label="资源组">
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
                                    { group_id: event.target.value || null },
                                  ),
                                "账号分组已更新。",
                              );
                            }}
                          >
                            <option value="">未分组</option>
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
                          label="允许项目使用"
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
                              "账号使用状态已更新。",
                            );
                          }}
                        />
                        <details className="channel-proxy">
                          <summary>更改此账号代理</summary>
                          <p>提交新代理会替换原配置。密码不会回显。</p>
                          <Field label="代理地址">
                            <Input
                              value={proxyServer}
                              onChange={(_, data) => setProxyServer(data.value)}
                            />
                          </Field>
                          <Field label="用户名">
                            <Input
                              value={proxyUsername}
                              onChange={(_, data) =>
                                setProxyUsername(data.value)
                              }
                              autoComplete="off"
                            />
                          </Field>
                          <Field label="密码">
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
                                "网络代理已更新。",
                              ).finally(() => {
                                setProxyPassword("");
                                setProxyUsername("");
                                setProxyServer("");
                              });
                            }}
                          >
                            保存代理
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
                                  "已恢复默认网络出口。",
                                );
                              }}
                            >
                              移除账号代理
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
        <section aria-label="项目发布连接器能力">
          <Card className="channel-card">
            <h2>项目发布连接器能力</h2>
            <p>
              账号已连接只代表登录身份有效；发布能力需独立完成真实发布与公开读回验证，并由运营方启用。
            </p>
            {capabilities.isPending && (
              <LoadingState label="正在加载连接器能力" />
            )}
            {capabilities.isError &&
              (capabilities.error instanceof ApiError &&
              capabilities.error.status === 403 ? (
                <ErrorState
                  title="权限不足"
                  detail="无权查看此项目的连接器能力。"
                />
              ) : (
                <ErrorState
                  detail={errorText(capabilities.error)}
                  onRetry={() => void capabilities.refetch()}
                />
              ))}
            {capabilities.data?.items.length === 0 && (
              <EmptyState
                title="尚无连接器能力"
                detail="尚未发现可核验的平台连接器；账号登录不会自动开放发布。"
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
                      {connectorState[item.availability]}
                    </Badge>
                  </div>
                  <p>
                    {item.availability === "available"
                      ? `当前可用发布格式：${item.content_types.map(publicationFormatLabel).join("、") || "无"}。`
                      : "当前没有可确认的发布内容类型；已有账号或旧配置不构成验证。"}
                  </p>
                  <p>
                    此处只读；项目账号或共享账号的连接状态不代表该平台已可发布。
                  </p>
                </li>
              ))}
            </ul>
          </Card>
        </section>
      )}
      <MessageBar intent="info">
        <MessageBarBody>
          Kimi 网页登录用于独立搜索测量；Kimi Code 的编码 OAuth
          属于不同能力，当前不在此处连接。
        </MessageBarBody>
      </MessageBar>
    </div>
  );
}

export function OperatorAccountsPage() {
  const { session } = useAuth();
  const client = useQueryClient();
  const allowed = Boolean(
    session?.memberships.some(
      (membership) =>
        membership.role === "operator_admin" ||
        membership.role === "resource_admin",
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
                      {accountState[account.status]}
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
