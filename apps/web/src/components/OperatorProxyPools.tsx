import { useEffect, useRef, useState } from "react";
import {
  Button,
  Card,
  Checkbox,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
  Spinner,
} from "@fluentui/react-components";
import { useInfiniteQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import "../i18n/proxyPools";
import { useAuth } from "../auth/AuthProvider";
import { ApiError } from "../api/client";
import {
  createProxyMember,
  createProxyPool,
  listProxyMembers,
  listProxyPools,
  patchProxyMember,
  patchProxyPool,
  type ProxyPool,
  type ProxyPoolMember,
} from "../api/proxyPools";

export function OperatorProxyPools() {
  const { session } = useAuth();
  const { t } = useTranslation("proxyPools");
  if (
    !session?.memberships.some((membership) =>
      ["operator_admin", "resource_admin"].includes(membership.role),
    )
  )
    return <p>{t("denied")}</p>;
  return (
    <ProxyPools
      key={`${session.user.id}:${session.operator.id}`}
      owner={[session.user.id, session.operator.id]}
    />
  );
}

function ProxyPools({ owner }: { owner: string[] }) {
  const { t } = useTranslation("proxyPools");
  const client = useQueryClient();
  const queryKey = ["operator-proxy-pools", ...owner];
  const pools = useInfiniteQuery({
    queryKey,
    queryFn: ({ pageParam }) => listProxyPools(pageParam),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page) => page.next_cursor ?? undefined,
    retry: false,
  });
  const [selected, setSelected] = useState("");
  const [poolEdit, setPoolEdit] = useState<ProxyPool>();
  const [poolName, setPoolName] = useState("");
  const [poolEnabled, setPoolEnabled] = useState(true);
  const [memberEdit, setMemberEdit] = useState<ProxyPoolMember>();
  const [memberName, setMemberName] = useState("");
  const [memberEnabled, setMemberEnabled] = useState(true);
  const [replace, setReplace] = useState(false);
  const [server, setServer] = useState("");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [conflict, setConflict] = useState(false);
  const [uncertain, setUncertain] = useState(false);
  const lock = useRef(false);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  const membersKey = [...queryKey, "members", selected];
  const members = useInfiniteQuery({
    queryKey: membersKey,
    queryFn: ({ pageParam }) => listProxyMembers(selected, pageParam),
    enabled: Boolean(selected),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page) => page.next_cursor ?? undefined,
    retry: false,
  });
  function clearSecrets() {
    setUsername("");
    setPassword("");
  }
  function resetMember() {
    setMemberEdit(undefined);
    setMemberName("");
    setMemberEnabled(true);
    setReplace(false);
    setServer("");
    clearSecrets();
    setConflict(false);
  }
  async function refresh() {
    await client.invalidateQueries({ queryKey });
  }
  // Mutation inputs never enter React Query's mutation cache.
  async function save(
    action: () => Promise<unknown>,
    accepted: () => void,
    creating: boolean,
  ) {
    if (lock.current || conflict || uncertain) return;
    lock.current = true;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      await action();
      if (!mounted.current) return;
      accepted();
      setNotice(t("saved"));
      await refresh();
    } catch (cause) {
      if (!mounted.current) return;
      const changed = cause instanceof ApiError && cause.status === 409;
      const unknown =
        creating &&
        (!(cause instanceof ApiError) ||
          cause.status === 0 ||
          (cause.status >= 200 && cause.status < 300) ||
          cause.status >= 500);
      setConflict(changed);
      setUncertain(unknown);
      setError(t(changed ? "conflict" : unknown ? "uncertain" : "failed"));
      if (changed || unknown) await refresh();
    } finally {
      lock.current = false;
      if (mounted.current) {
        clearSecrets();
        setBusy(false);
      }
    }
  }
  function saveMember() {
    const replacing = !memberEdit || replace;
    if (replacing) {
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
          parsed.username ||
          parsed.password ||
          parsed.search ||
          parsed.hash ||
          (parsed.pathname && parsed.pathname !== "/")
        )
          throw new Error();
      } catch {
        setError(t("invalidServer"));
        clearSecrets();
        return;
      }
    }
    const proxy = {
      server: server.trim(),
      ...(username ? { username } : {}),
      ...(password ? { password } : {}),
    };
    void save(
      () =>
        memberEdit
          ? patchProxyMember(selected, memberEdit.member_id, {
              expected_version: memberEdit.version,
              name: memberName.trim(),
              enabled: memberEnabled,
              ...(replace ? { proxy } : {}),
            })
          : createProxyMember(selected, { name: memberName.trim(), proxy }),
      resetMember,
      !memberEdit,
    );
  }
  const blocked = busy || conflict || uncertain;
  return (
    <Card className="channel-card" aria-label={t("title")}>
      <h2>{t("title")}</h2>
      <p>{t("help")}</p>
      {error && (
        <MessageBar intent="error">
          <MessageBarBody>{error}</MessageBarBody>
        </MessageBar>
      )}
      {notice && <p role="status">{notice}</p>}
      {(pools.isPending || (selected && members.isPending)) && (
        <Spinner size="tiny" label={t("loading")} />
      )}
      {(pools.isError || (selected && members.isError)) && (
        <p role="alert">{t("loadFailed")}</p>
      )}
      <Button
        disabled={busy}
        onClick={async () => {
          const results = await Promise.all([
            pools.refetch(),
            ...(selected ? [members.refetch()] : []),
          ]);
          if (mounted.current && results.every((result) => !result.isError))
            setUncertain(false);
        }}
      >
        {t("reload")}
      </Button>
      {pools.data?.pages[0].items.length === 0 && <p>{t("empty")}</p>}
      <ul className="channel-accounts">
        {pools.data?.pages
          .flatMap((page) => page.items)
          .map((pool) => (
            <li key={pool.pool_id}>
              <strong>{pool.name}</strong> ·{" "}
              {t(pool.enabled ? "enabled" : "disabled")}
              <Button
                disabled={busy}
                onClick={() => {
                  setSelected(pool.pool_id);
                  resetMember();
                }}
              >
                {t("select")}
              </Button>
              <Button
                disabled={busy}
                onClick={() => {
                  setPoolEdit(pool);
                  setPoolName(pool.name);
                  setPoolEnabled(pool.enabled);
                  setConflict(false);
                  clearSecrets();
                }}
              >
                {t("edit")}
              </Button>
            </li>
          ))}
      </ul>
      {pools.hasNextPage && (
        <Button
          disabled={pools.isFetching || busy}
          onClick={() => void pools.fetchNextPage()}
        >
          {t("more")}
        </Button>
      )}
      <div className="channel-form">
        <Field label={t("poolName")}>
          <Input
            value={poolName}
            disabled={busy}
            maxLength={160}
            onChange={(_, data) => setPoolName(data.value)}
          />
        </Field>
        {poolEdit && (
          <Checkbox
            label={t("enabled")}
            checked={poolEnabled}
            disabled={busy}
            onChange={(_, data) => setPoolEnabled(data.checked === true)}
          />
        )}
        <Button
          disabled={blocked || !poolName.trim()}
          onClick={() =>
            void save(
              () =>
                poolEdit
                  ? patchProxyPool(poolEdit.pool_id, {
                      expected_version: poolEdit.version,
                      name: poolName.trim(),
                      enabled: poolEnabled,
                    })
                  : createProxyPool(poolName.trim()),
              () => {
                setPoolName("");
                setPoolEdit(undefined);
              },
              !poolEdit,
            )
          }
        >
          {t(poolEdit ? "save" : "create")}
        </Button>
        {poolEdit && (
          <Button
            disabled={busy}
            onClick={() => {
              setPoolEdit(undefined);
              setPoolName("");
              setConflict(false);
            }}
          >
            {t("cancel")}
          </Button>
        )}
      </div>
      {selected && (
        <section aria-label={t("select")}>
          <h3>
            {
              pools.data?.pages
                .flatMap((page) => page.items)
                .find((pool) => pool.pool_id === selected)?.name
            }
          </h3>
          {members.data?.pages[0].items.length === 0 && (
            <p>{t("emptyMembers")}</p>
          )}
          <ul className="channel-accounts">
            {members.data?.pages
              .flatMap((page) => page.items)
              .map((member) => (
                <li key={member.member_id}>
                  <strong>{member.name}</strong> ·{" "}
                  {t(member.enabled ? "enabled" : "disabled")}
                  <p>{member.proxy_server}</p>
                  <Button
                    disabled={busy}
                    onClick={() => {
                      resetMember();
                      setMemberEdit(member);
                      setMemberName(member.name);
                      setMemberEnabled(member.enabled);
                      setServer(member.proxy_server);
                    }}
                  >
                    {t("edit")}
                  </Button>
                </li>
              ))}
          </ul>
          {members.hasNextPage && (
            <Button
              disabled={members.isFetching || busy}
              onClick={() => void members.fetchNextPage()}
            >
              {t("more")}
            </Button>
          )}
          <div className="channel-form">
            <Field label={t("memberName")}>
              <Input
                value={memberName}
                maxLength={160}
                disabled={busy}
                onChange={(_, data) => setMemberName(data.value)}
              />
            </Field>
            {memberEdit && (
              <>
                <Checkbox
                  label={t("enabled")}
                  checked={memberEnabled}
                  disabled={busy}
                  onChange={(_, data) =>
                    setMemberEnabled(data.checked === true)
                  }
                />
                <Checkbox
                  label={t("replace")}
                  checked={replace}
                  disabled={busy}
                  onChange={(_, data) => {
                    setReplace(data.checked === true);
                    clearSecrets();
                  }}
                />
              </>
            )}
            {(!memberEdit || replace) && (
              <>
                <Field label={t("server")} hint={t("serverHelp")}>
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
              disabled={
                blocked ||
                !memberName.trim() ||
                ((!memberEdit || replace) && !server.trim())
              }
              onClick={saveMember}
            >
              {t(memberEdit ? "save" : "add")}
            </Button>
            {memberEdit && (
              <Button disabled={busy} onClick={resetMember}>
                {t("cancel")}
              </Button>
            )}
          </div>
        </section>
      )}
    </Card>
  );
}
