import { useEffect, useRef, useState } from "react";
import { Button, Card, Field, Input, Switch } from "@fluentui/react-components";
import {
  useInfiniteQuery,
  useQueryClient,
  type InfiniteData,
} from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { useLocation } from "react-router-dom";
import { getAuthGeneration } from "../api/client";
import * as api from "../api/partnerConnections";
import { useAuth } from "../auth/AuthProvider";
import { membershipForTenant } from "../auth/types";
import { formatUiDate } from "../i18n";

type Scope = { tenantId: string; projectId: string; identity: string };
function useLifetime() {
  const alive = useRef(true);
  const generation = useRef(getAuthGeneration());
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  return () => alive.current && generation.current === getAuthGeneration();
}

function ConnectionCard({
  tenantId,
  projectId,
  identity,
  connection,
  refresh,
}: Scope & {
  connection: api.PartnerConnection;
  refresh: (updated?: api.PartnerConnection) => Promise<unknown>;
}) {
  const { t } = useTranslation("partnerConnections");
  const current = useLifetime();
  const [secret, setSecret] = useState<string>();
  const [notice, setNotice] = useState<string>();
  const [busy, setBusy] = useState(false);
  const lock = useRef(false);
  const [expiry, setExpiry] = useState("");
  const [uncertain, setUncertain] = useState(false);
  const query = useInfiniteQuery({
    queryKey: [
      "partner-keys",
      identity,
      tenantId,
      projectId,
      connection.connection_id,
    ],
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) =>
      api.listPartnerKeys(
        tenantId,
        projectId,
        connection.connection_id,
        pageParam,
      ),
    getNextPageParam: (page) => page.next_cursor ?? undefined,
    retry: false,
    gcTime: 0,
  });
  const keys = query.data?.pages.flatMap((page) => page.items) ?? [];
  const expiresAt = expiry ? new Date(expiry).getTime() : null;
  const validExpiry =
    expiresAt === null ||
    (Number.isFinite(expiresAt) && expiresAt > Date.now());
  const revoked = Boolean(connection.revoked_at);
  async function changeExportPermission(enabled: boolean) {
    if (lock.current || revoked || !current()) return;
    lock.current = true;
    setBusy(true);
    setNotice(undefined);
    try {
      const updated = await api.setPartnerContentExport(
        tenantId,
        projectId,
        connection.connection_id,
        enabled,
      );
      if (!current()) return;
      await refresh(updated);
    } catch {
      if (!current()) return;
      setNotice("permissionFailed");
      await refresh();
    } finally {
      lock.current = false;
      if (current()) setBusy(false);
    }
  }
  async function act(
    action: "issue" | "rotate" | "revoke" | "connection",
    keyId?: string,
  ) {
    if (lock.current || !current()) return;
    lock.current = true;
    setBusy(true);
    setSecret(undefined);
    setNotice(undefined);
    const issuing = action === "issue" || action === "rotate";
    try {
      if (issuing) {
        const date =
          expiresAt === null ? null : new Date(expiresAt).toISOString();
        const result =
          action === "issue"
            ? await api.issuePartnerKey(
                tenantId,
                projectId,
                connection.connection_id,
                date,
              )
            : await api.rotatePartnerKey(
                tenantId,
                projectId,
                connection.connection_id,
                keyId!,
                date,
              );
        if (!current()) return;
        setSecret(result.bearer);
      } else if (action === "connection") {
        await api.revokePartnerConnection(
          tenantId,
          projectId,
          connection.connection_id,
        );
      } else {
        await api.revokePartnerKey(
          tenantId,
          projectId,
          connection.connection_id,
          keyId!,
        );
      }
      if (!current()) return;
      await query.refetch();
      if (!current()) return;
      await refresh();
    } catch {
      if (!current()) return;
      setNotice(issuing ? "uncertain" : "failed");
      if (issuing) setUncertain(true);
      await query.refetch();
    } finally {
      lock.current = false;
      if (current()) setBusy(false);
    }
  }
  async function copy() {
    if (!secret || !current()) return;
    try {
      await navigator.clipboard.writeText(secret);
      if (current()) setNotice("copied");
    } catch {
      if (current()) setNotice("copyFailed");
    }
  }
  return (
    <Card className="channel-card" aria-label={connection.connection_id}>
      <h3>{connection.connection_id}</h3>
      <p>
        {t(revoked ? "revoked" : "active")} ·{" "}
        {t("created", { date: formatUiDate(connection.created_at) })}
      </p>
      {!connection.knowledge_write && <p>{t("restricted")}</p>}
      <Field hint={t(revoked ? "exportRevoked" : "exportHint")}>
        <Switch
          label={t("allowExport")}
          checked={connection.content_export}
          disabled={busy || revoked}
          onChange={(_, data) => void changeExportPermission(data.checked)}
        />
      </Field>
      <h4>{t("keys")}</h4>
      {query.isPending && <p role="status">{t("loading")}</p>}
      {query.isError && <p role="alert">{t("loadFailed")}</p>}
      {query.isSuccess && !keys.length && <p>{t("noKeys")}</p>}
      {keys.map((key) => {
        const state =
          revoked || key.revoked_at
            ? "revoked"
            : key.expires_at && new Date(key.expires_at).getTime() <= Date.now()
              ? "expired"
              : "active";
        return (
          <div key={key.key_id}>
            <p>
              {key.key_id} · {t(state)} ·{" "}
              {key.expires_at
                ? t("expires", { date: formatUiDate(key.expires_at) })
                : t("neverExpires")}
            </p>
            {!revoked && !key.revoked_at && (
              <>
                <Button
                  disabled={busy || !validExpiry}
                  onClick={() => void act("rotate", key.key_id)}
                >
                  {t("rotate")}
                </Button>
                <Button
                  disabled={busy}
                  onClick={() => void act("revoke", key.key_id)}
                >
                  {t("revoke")}
                </Button>
              </>
            )}
          </div>
        );
      })}
      {query.hasNextPage && (
        <Button
          disabled={query.isFetching}
          onClick={() => void query.fetchNextPage()}
        >
          {t("more")}
        </Button>
      )}
      <Button
        disabled={busy || query.isFetching}
        onClick={() => void query.refetch()}
      >
        {t("reload")}
      </Button>
      {!revoked && (
        <>
          <Field
            label={t("expiry")}
            hint={t("noExpiry")}
            validationMessage={!validExpiry ? t("invalidExpiry") : undefined}
          >
            <Input
              type="datetime-local"
              value={expiry}
              disabled={busy}
              onChange={(_, data) => setExpiry(data.value)}
            />
          </Field>
          <p>{t("rotateHint")}</p>
          <Button
            disabled={busy || !validExpiry}
            onClick={() => void act("issue")}
          >
            {t("createKey")}
          </Button>
          <Button disabled={busy} onClick={() => void act("connection")}>
            {t("revokeConnection")}
          </Button>
        </>
      )}
      {uncertain && <p role="alert">{t("uncertain")}</p>}
      {notice && notice !== "uncertain" && (
        <p role={notice === "permissionFailed" ? "alert" : "status"}>
          {t(notice)}
        </p>
      )}
      {secret && current() && !revoked && (
        <div role="region" aria-label={t("secret")}>
          <p>{t("oneTime")}</p>
          <Field label={t("secret")}>
            <Input readOnly value={secret} autoComplete="off" />
          </Field>
          <Button onClick={() => void copy()}>{t("copy")}</Button>
          <Button
            onClick={() => {
              setSecret(undefined);
              setNotice(undefined);
            }}
          >
            {t("dismiss")}
          </Button>
        </div>
      )}
    </Card>
  );
}

function ScopedPanel({ tenantId, projectId, identity }: Scope) {
  const { t } = useTranslation("partnerConnections");
  const current = useLifetime();
  const queryClient = useQueryClient();
  const [busy, setBusy] = useState(false);
  const lock = useRef(false);
  const [failed, setFailed] = useState(false);
  const queryKey = ["partner-connections", identity, tenantId, projectId];
  const query = useInfiniteQuery({
    queryKey,
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) =>
      api.listPartnerConnections(tenantId, projectId, pageParam),
    getNextPageParam: (page) => page.next_cursor ?? undefined,
    retry: false,
    gcTime: 0,
  });
  async function add() {
    if (lock.current || !current()) return;
    lock.current = true;
    setBusy(true);
    setFailed(false);
    try {
      await api.createPartnerConnection(tenantId, projectId);
    } catch {
      if (current()) setFailed(true);
    } finally {
      lock.current = false;
      if (current()) {
        setBusy(false);
        await query.refetch();
      }
    }
  }
  const connections = query.data?.pages.flatMap((page) => page.items) ?? [];
  return (
    <>
      <Button disabled={busy} onClick={() => void add()}>
        {t("add")}
      </Button>
      <Button disabled={query.isFetching} onClick={() => void query.refetch()}>
        {t("reload")}
      </Button>
      {failed && <p role="alert">{t("failed")}</p>}
      {query.isPending && <p role="status">{t("loading")}</p>}
      {query.isError && <p role="alert">{t("loadFailed")}</p>}
      {query.isSuccess && !connections.length && <p>{t("empty")}</p>}
      {connections.map((connection) => (
        <ConnectionCard
          key={connection.connection_id}
          tenantId={tenantId}
          projectId={projectId}
          identity={identity}
          connection={connection}
          refresh={(updated) => {
            if (updated) {
              queryClient.setQueryData<
                InfiniteData<api.PartnerPage<api.PartnerConnection>>
              >(queryKey, (data) =>
                data
                  ? {
                      ...data,
                      pages: data.pages.map((page) => ({
                        ...page,
                        items: page.items.map((item) =>
                          item.connection_id === updated.connection_id
                            ? updated
                            : item,
                        ),
                      })),
                    }
                  : data,
              );
            }
            return query.refetch();
          }}
        />
      ))}
      {query.hasNextPage && (
        <Button
          disabled={query.isFetching}
          onClick={() => void query.fetchNextPage()}
        >
          {t("more")}
        </Button>
      )}
    </>
  );
}
export function PartnerConnectionsPanel({
  tenantId,
  projectId,
}: {
  tenantId: string;
  projectId: string;
}) {
  const { session, status } = useAuth();
  const location = useLocation();
  const { t } = useTranslation("partnerConnections");
  const canWrite =
    status === "authenticated" &&
    membershipForTenant(session, tenantId)?.role === "tenant_admin";
  const identity = JSON.stringify([
    session?.user.id,
    session?.operator.id,
    getAuthGeneration(),
    tenantId,
    projectId,
    location.key,
  ]);
  return (
    <section className="channel-stack" aria-label={t("title")}>
      <h2>{t("title")}</h2>
      <p>{t("description")}</p>
      {canWrite ? (
        <ScopedPanel
          key={identity}
          tenantId={tenantId}
          projectId={projectId}
          identity={identity}
        />
      ) : (
        <p>{t("adminOnly")}</p>
      )}
    </section>
  );
}
