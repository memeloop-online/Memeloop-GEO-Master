import { useEffect, useRef, useState } from "react";
import {
  Button,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
  Select,
  Spinner,
} from "@fluentui/react-components";
import {
  useInfiniteQuery,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import "../i18n/accountNetworks";
import { useAuth } from "../auth/AuthProvider";
import { ApiError } from "../api/client";
import {
  getAccountNetwork,
  saveAccountNetwork,
  type NetworkSource,
  type SavedNetwork,
} from "../api/accountNetworks";
import { listProxyMembers, listProxyPools } from "../api/proxyPools";

export function OperatorAccountNetwork({ accountId }: { accountId: string }) {
  const { session } = useAuth();
  const { t } = useTranslation("accountNetworks");
  if (
    !session?.memberships.some((entry) =>
      ["operator_admin", "resource_admin"].includes(entry.role),
    )
  )
    return null;
  return (
    <NetworkDisclosure
      key={`${session.user.id}:${session.operator.id}:${accountId}`}
      accountId={accountId}
      owner={[session.user.id, session.operator.id]}
      title={t("title")}
    />
  );
}

function NetworkDisclosure({
  accountId,
  owner,
  title,
}: {
  accountId: string;
  owner: string[];
  title: string;
}) {
  const [open, setOpen] = useState(false);
  return (
    <details
      className="channel-proxy"
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary>{title}</summary>
      {open && <NetworkEditor accountId={accountId} owner={owner} />}
    </details>
  );
}

function NetworkEditor({
  accountId,
  owner,
}: {
  accountId: string;
  owner: string[];
}) {
  const { t } = useTranslation("accountNetworks");
  const client = useQueryClient();
  const key = ["operator-account-network", ...owner, accountId];
  const network = useQuery({
    queryKey: key,
    queryFn: () => getAccountNetwork(accountId),
    retry: false,
  });
  const [mode, setMode] = useState<NetworkSource["kind"]>("pool_revision");
  const [poolId, setPoolId] = useState("");
  const [memberRevision, setMemberRevision] = useState("");
  const [server, setServer] = useState("");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [blocked, setBlocked] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const mounted = useRef(true);
  const locked = useRef(false);
  const editingVersion = useRef<number | undefined>(undefined);
  useEffect(() => {
    if (editingVersion.current === undefined && network.data)
      editingVersion.current = network.data.version;
  }, [network.data]);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  const pools = useInfiniteQuery({
    queryKey: ["operator-proxy-pools", ...owner],
    queryFn: ({ pageParam }) => listProxyPools(pageParam),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page) => page.next_cursor ?? undefined,
    enabled: mode === "pool_revision",
    retry: false,
  });
  const members = useInfiniteQuery({
    queryKey: ["operator-proxy-pools", ...owner, "members", poolId],
    queryFn: ({ pageParam }) => listProxyMembers(poolId, pageParam),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page) => page.next_cursor ?? undefined,
    enabled: mode === "pool_revision" && Boolean(poolId),
    retry: false,
  });
  const poolItems = pools.data?.pages.flatMap((page) => page.items) ?? [];
  const memberItems = members.data?.pages.flatMap((page) => page.items) ?? [];
  // Selection includes the immutable revision. Refetching a rotated member never
  // silently changes the revision the operator chose.
  const selectedMember = memberItems.find(
    (member) =>
      `${member.member_id}:${member.revision_id}` === memberRevision &&
      member.enabled,
  );
  const selectedPool = poolItems.find(
    (pool) => pool.pool_id === poolId && pool.enabled,
  );
  function clearSecrets() {
    setUsername("");
    setPassword("");
  }
  function describe(saved: SavedNetwork) {
    if (
      saved.source.kind === "direct" ||
      (saved.source.kind === "legacy_snapshot" && !saved.server)
    )
      return t("direct");
    return [
      saved.pool_name,
      saved.member_name ??
        t(saved.source.kind === "pool_revision" ? "pool" : "configured"),
      saved.server,
    ]
      .filter(Boolean)
      .join(" · ");
  }
  function configurationReference(saved: SavedNetwork) {
    const reference = saved.source.revision_id ?? saved.binding_id;
    if (!reference) return null;
    const other =
      saved === network.data?.active
        ? network.data?.pending
        : network.data?.active;
    const otherReference = other?.source.revision_id ?? other?.binding_id;
    let length = 8;
    // Short references stay distinguishable even when two configurations share
    // the same address and happen to have the same identifier prefix.
    if (otherReference && otherReference !== reference)
      while (
        length < reference.length &&
        reference.slice(0, length) === otherReference.slice(0, length)
      )
        length += 1;
    return (
      <small title={reference}>
        {t("configuration", { reference: reference.slice(0, length) })}
      </small>
    );
  }
  async function reload() {
    if (locked.current) return;
    locked.current = true;
    setBusy(true);
    clearSecrets();
    try {
      const result = await network.refetch();
      if (mode === "pool_revision") {
        await pools.refetch();
        if (poolId) await members.refetch();
      }
      if (!mounted.current) return;
      if (!result.isError) {
        editingVersion.current = result.data?.version;
        setBlocked(false);
        setError("");
      }
    } finally {
      locked.current = false;
      if (mounted.current) setBusy(false);
    }
  }
  async function save() {
    if (locked.current || blocked || !network.data || network.isError) return;
    let source: NetworkSource;
    if (mode === "pool_revision") {
      if (!selectedPool || !selectedMember) return;
      source = {
        kind: mode,
        pool_id: poolId,
        member_id: selectedMember.member_id,
        revision_id: selectedMember.revision_id,
      };
    } else if (mode === "direct") source = { kind: "direct" };
    else {
      try {
        const parsed = new URL(server.trim());
        if (parsed.username || parsed.password) {
          setServer("");
          throw new Error();
        }
        if (
          !["http:", "https:", "socks5:"].includes(parsed.protocol) ||
          !parsed.hostname ||
          parsed.port === "0" ||
          (parsed.protocol === "socks5:" && !parsed.port) ||
          parsed.search ||
          parsed.hash ||
          (parsed.pathname && parsed.pathname !== "/")
        )
          throw new Error();
      } catch {
        clearSecrets();
        setError(t("invalid"));
        return;
      }
      source = {
        kind: "standalone",
        proxy: {
          server: server.trim(),
          ...(username ? { username } : {}),
          ...(password ? { password } : {}),
        },
      };
    }
    locked.current = true;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      // Do not retain credential-bearing inputs in a mutation cache.
      const saved = await saveAccountNetwork(accountId, {
        expected_version: editingVersion.current ?? network.data.version,
        source,
      });
      if (!mounted.current) return;
      if (
        !saved?.active ||
        saved.account_id !== accountId ||
        !Number.isSafeInteger(saved.version)
      )
        throw new ApiError(200, {}, "invalid response");
      client.setQueryData(key, saved);
      editingVersion.current = saved.version;
      setNotice(t(saved.pending ? "queued" : "saved"));
      void client.invalidateQueries({
        queryKey: ["operator-channel-accounts", ...owner],
      });
    } catch (cause) {
      if (!mounted.current) return;
      const conflict = cause instanceof ApiError && cause.status === 409;
      const unknown =
        !(cause instanceof ApiError) ||
        cause.status === 0 ||
        (cause.status >= 200 && cause.status < 300) ||
        cause.status >= 500;
      setBlocked(conflict || unknown);
      setError(t(conflict ? "conflict" : unknown ? "uncertain" : "failed"));
    } finally {
      locked.current = false;
      if (mounted.current) {
        clearSecrets();
        setBusy(false);
      }
    }
  }
  const inventoryError =
    mode === "pool_revision" && (pools.isError || (poolId && members.isError));
  return (
    <section aria-label={t("title")}>
      {network.isPending && <Spinner size="tiny" label={t("loading")} />}
      {(network.isError || inventoryError) && (
        <p role="alert">{t("loadFailed")}</p>
      )}
      {network.data && (
        <>
          <p>
            {t("active")}: {describe(network.data.active)}{" "}
            {configurationReference(network.data.active)}
          </p>
          {network.data.pending && (
            <>
              <p>
                {t("pending")}: {describe(network.data.pending)}{" "}
                {configurationReference(network.data.pending)}
              </p>
              <p>{t("pendingHelp")}</p>
            </>
          )}
        </>
      )}
      {error && (
        <MessageBar intent="error">
          <MessageBarBody>{error}</MessageBarBody>
        </MessageBar>
      )}
      {notice && <p role="status">{notice}</p>}
      <Button disabled={busy} onClick={() => void reload()}>
        {t("reload")}
      </Button>
      <div className="channel-form">
        <Field label={t("source")}>
          <Select
            value={mode}
            disabled={busy}
            onChange={(_, data) => {
              setMode(data.value as NetworkSource["kind"]);
              clearSecrets();
              setNotice("");
            }}
          >
            <option value="pool_revision">{t("pool")}</option>
            <option value="standalone">{t("manual")}</option>
            <option value="direct">{t("direct")}</option>
          </Select>
        </Field>
        {mode === "pool_revision" && (
          <>
            <Field label={t("poolLabel")}>
              <Select
                value={poolId}
                disabled={busy || pools.isPending}
                onChange={(_, data) => {
                  setPoolId(data.value);
                  setMemberRevision("");
                }}
              >
                <option value="">{t("choosePool")}</option>
                {poolItems.map((pool) => (
                  <option
                    key={pool.pool_id}
                    value={pool.pool_id}
                    disabled={!pool.enabled}
                  >
                    {pool.name}
                    {!pool.enabled ? ` (${t("disabled")})` : ""}
                  </option>
                ))}
              </Select>
            </Field>
            {pools.isSuccess && !poolItems.some((pool) => pool.enabled) && (
              <p>{t("noPools")}</p>
            )}
            {pools.hasNextPage && (
              <Button
                disabled={busy || pools.isFetching}
                onClick={() => void pools.fetchNextPage()}
              >
                {t("more")}
              </Button>
            )}
            {poolId && (
              <>
                <Field label={t("memberLabel")}>
                  <Select
                    value={memberRevision}
                    disabled={busy || members.isPending}
                    onChange={(_, data) => setMemberRevision(data.value)}
                  >
                    <option value="">{t("chooseMember")}</option>
                    {memberItems.map((member) => (
                      <option
                        key={member.member_id}
                        value={`${member.member_id}:${member.revision_id}`}
                        disabled={!member.enabled}
                      >
                        {member.name}
                        {!member.enabled ? ` (${t("disabled")})` : ""}
                      </option>
                    ))}
                  </Select>
                </Field>
                {members.isSuccess &&
                  !memberItems.some((member) => member.enabled) && (
                    <p>{t("noMembers")}</p>
                  )}
                {members.hasNextPage && (
                  <Button
                    disabled={busy || members.isFetching}
                    onClick={() => void members.fetchNextPage()}
                  >
                    {t("more")}
                  </Button>
                )}
              </>
            )}
          </>
        )}
        {mode === "standalone" && (
          <>
            <Field label={t("server")}>
              <Input
                value={server}
                disabled={busy}
                autoComplete="off"
                onChange={(_, data) => setServer(data.value)}
              />
            </Field>
            <p>{t("authHelp")}</p>
            <Field label={t("username")}>
              <Input
                value={username}
                disabled={busy}
                autoComplete="off"
                onChange={(_, data) => setUsername(data.value)}
              />
            </Field>
            <Field label={t("password")}>
              <Input
                type="password"
                value={password}
                disabled={busy}
                autoComplete="new-password"
                onChange={(_, data) => setPassword(data.value)}
              />
            </Field>
          </>
        )}
        <Button
          appearance="primary"
          disabled={
            busy ||
            blocked ||
            !network.data ||
            network.isError ||
            Boolean(inventoryError) ||
            (mode === "pool_revision" && (!selectedPool || !selectedMember)) ||
            (mode === "standalone" && !server.trim())
          }
          onClick={() => void save()}
        >
          {t("save")}
        </Button>
      </div>
    </section>
  );
}
