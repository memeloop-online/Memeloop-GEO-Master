import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  Button,
  Card,
  MessageBar,
  MessageBarBody,
  Spinner,
} from "@fluentui/react-components";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import "../i18n/loginQueues";
import { useAuth } from "../auth/AuthProvider";
import { ApiError, createIdempotencyKey } from "../api/client";
import {
  actOnLoginQueue,
  createLoginQueue,
  getActiveLoginQueue,
  getLoginQueue,
  type LoginQueueAction,
  type LoginQueueSelection,
  type LoginQueueView,
} from "../api/loginQueues";

export interface QueueDesktopControls {
  sessionId: string;
  complete: () => Promise<void>;
  pause: () => Promise<void>;
}
interface Props {
  selection?: LoginQueueSelection;
  accountNames?: Record<string, string>;
  directBusy: boolean;
  renderDesktop: (controls: QueueDesktopControls) => ReactNode;
  onActiveChange?: (active: boolean) => void;
}
export function OperatorLoginQueue(props: Props) {
  const { session } = useAuth();
  // The synchronizer token changes with authentication. Use an unrelated
  // in-memory nonce to partition session-owned desktop projections; never put
  // the token itself (or a token digest) in query keys or browser storage.
  const sessionScope = useMemo(
    () => createIdempotencyKey(),
    [session?.user.id, session?.operator.id, session?.csrf_token],
  );
  if (
    !session?.memberships.some((entry) =>
      ["operator_admin", "resource_admin"].includes(entry.role),
    )
  )
    return null;
  return (
    <Queue
      key={sessionScope}
      {...props}
      owner={[session.user.id, session.operator.id, sessionScope]}
    />
  );
}
function Queue({
  selection,
  accountNames = {},
  directBusy,
  renderDesktop,
  onActiveChange,
  owner,
}: Props & { owner: string[] }) {
  const { t } = useTranslation("loginQueues");
  const client = useQueryClient();
  const [id, setId] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [blocked, setBlocked] = useState(false);
  const [pendingCreate, setPendingCreate] = useState<
    LoginQueueSelection & { queue_id: string }
  >();
  const lock = useRef(false);
  const mounted = useRef(true);
  const driven = useRef("");
  const prefix = ["operator-login-queue", ...owner];
  const queryKey = [...prefix, id ?? "active"];
  const query = useQuery({
    queryKey,
    queryFn: () => (id ? getLoginQueue(id) : getActiveLoginQueue()),
    retry: false,
    gcTime: 0,
    structuralSharing: (previous, incoming) => {
      const old = previous as LoginQueueView | null | undefined;
      const next = incoming as LoginQueueView | null | undefined;
      return old &&
        next &&
        old.queue_id === next.queue_id &&
        old.version > next.version
        ? old
        : incoming;
    },
    refetchInterval: (entry) =>
      busy || entry.state.data?.status === "completed" ? false : 2000,
  });
  const queue = query.data;
  // Until recovery resolves, or an ambiguous create is reconciled, the shared
  // login surface may already own a browser even without a local projection.
  const reservesLogin =
    Boolean(queue && queue.status !== "completed") ||
    (!queue && (query.isPending || query.isError)) ||
    Boolean(pendingCreate);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  useEffect(() => {
    if (queue && !id) {
      client.setQueryData([...prefix, queue.queue_id], queue);
      setId(queue.queue_id);
    }
  }, [queue, id, client, owner[0], owner[1]]);
  useEffect(() => {
    onActiveChange?.(reservesLogin);
    return () => onActiveChange?.(false);
  }, [reservesLogin, onActiveChange]);
  function accept(next: LoginQueueView) {
    if (
      !next?.queue_id ||
      !Array.isArray(next.items) ||
      !Number.isSafeInteger(next.version)
    )
      throw new ApiError(200, {}, "invalid response");
    client.setQueryData([...prefix, next.queue_id], next);
    setId(next.queue_id);
    setPendingCreate((pending) =>
      pending?.queue_id === next.queue_id ? undefined : pending,
    );
    void client.invalidateQueries({
      queryKey: ["operator-channel-accounts", ...owner.slice(0, 2)],
    });
  }
  async function action(kind: LoginQueueAction) {
    if (lock.current || !queue || blocked || (directBusy && kind !== "pause"))
      throw new Error(t("failed"));
    lock.current = true;
    setBusy(true);
    setError("");
    try {
      await client.cancelQueries({ queryKey: prefix });
      const next = await actOnLoginQueue(queue.queue_id, kind, queue.version);
      if (mounted.current) accept(next);
    } catch (cause) {
      if (mounted.current) {
        setBlocked(true);
        setError(
          t(
            cause instanceof ApiError && cause.status === 409
              ? "conflict"
              : "failed",
          ),
        );
        await query.refetch();
      }
      throw new Error(t("failed"));
    } finally {
      lock.current = false;
      if (mounted.current) setBusy(false);
    }
  }
  useEffect(() => {
    if (
      !queue ||
      queue.status !== "running" ||
      !queue.can_drive ||
      queue.current?.requires_session_recovery ||
      directBusy ||
      busy ||
      blocked ||
      query.isError
    )
      return;
    const attempt = `${queue.queue_id}:${queue.version}:${queue.current?.phase ?? "next"}`;
    if (driven.current === attempt) return;
    driven.current = attempt;
    void action("drive").catch(() => {});
  }, [queue, directBusy, busy, blocked, query.isError]);
  async function start() {
    if (
      lock.current ||
      directBusy ||
      (queue && queue.status !== "completed") ||
      !selection?.items.length
    )
      return;
    const request = pendingCreate ?? {
      ...selection,
      items: [...selection.items],
      queue_id: createIdempotencyKey(),
    };
    setPendingCreate(request);
    lock.current = true;
    setBusy(true);
    setError("");
    try {
      const next = await createLoginQueue(request);
      if (mounted.current) {
        accept(next);
        setPendingCreate(undefined);
        setBlocked(false);
      }
    } catch (cause) {
      if (mounted.current) {
        setError(
          t(
            cause instanceof ApiError && cause.status === 409
              ? "conflict"
              : "failed",
          ),
        );
        // An ambiguous create is read back; explicit retries retain its ID and
        // frozen ordering, never silently create another queue.
        const active = await getActiveLoginQueue().catch(() => undefined);
        if (mounted.current && active) accept(active);
      }
    } finally {
      lock.current = false;
      if (mounted.current) setBusy(false);
    }
  }
  const current = queue?.current;
  const canShowDesktop =
    current?.session_id &&
    !current.requires_session_recovery &&
    current.phase === "awaiting_login" &&
    queue?.status === "running" &&
    !directBusy;
  return (
    <Card className="channel-card" aria-label={t("title")}>
      <h2>{t("title")}</h2>
      <p>{t("help")}</p>
      {query.isPending && <Spinner size="tiny" label={t("loading")} />}
      {directBusy && <p>{t("directBusy")}</p>}
      {(error || query.isError) && (
        <MessageBar intent="error">
          <MessageBarBody>{error || t("failed")}</MessageBarBody>
        </MessageBar>
      )}
      <Button
        disabled={busy}
        onClick={async () => {
          const result = await query.refetch();
          if (mounted.current && !result.isError) {
            setBlocked(false);
            setError("");
          }
        }}
      >
        {t("reload")}
      </Button>
      {queue && (
        <>
          <p role="status">
            {t(queue.status)} ·{" "}
            {t("progress", {
              done: queue.items.filter((item) => item.status === "succeeded")
                .length,
              total: queue.items.length,
            })}
          </p>
          <ol>
            {queue.items.map((item) => (
              <li key={item.position}>
                {t("row", { number: item.position + 1 })} ·{" "}
                {accountNames[item.account_id] &&
                  `${accountNames[item.account_id]} · `}
                {t(item.status === "current" ? "currentStatus" : item.status)}
                {item.error_code && <p>{t("savedError")}</p>}
              </li>
            ))}
          </ol>
          <div className="channel-row">
            {queue.status === "running" && (
              <Button
                disabled={busy || blocked}
                onClick={() => void action("pause").catch(() => {})}
              >
                {t("pause")}
              </Button>
            )}
            {queue.status === "paused" && (
              <Button
                disabled={busy || blocked || directBusy}
                onClick={() => void action("resume").catch(() => {})}
              >
                {t("resume")}
              </Button>
            )}
            {current && (
              <>
                <Button
                  disabled={busy || blocked || directBusy}
                  onClick={() => void action("retry").catch(() => {})}
                >
                  {t("retry")}
                </Button>
                <Button
                  disabled={busy || blocked || directBusy}
                  onClick={() => void action("skip").catch(() => {})}
                >
                  {t("skip")}
                </Button>
              </>
            )}
            {queue.status === "running" && queue.can_drive && (
              <Button
                disabled={busy || blocked || directBusy}
                onClick={() => void action("drive").catch(() => {})}
              >
                {t("drive")}
              </Button>
            )}
          </div>
          {current && <h3>{t("current", { number: current.position + 1 })}</h3>}
          {current?.phase === "closing" && <p>{t("closing")}</p>}
          {(current?.requires_session_recovery ||
            current?.phase === "recovery_required") && <p>{t("recovery")}</p>}
          {current?.phase === "preparing" && <p>{t("preparing")}</p>}
          {canShowDesktop &&
            renderDesktop({
              sessionId: current.session_id!,
              complete: () => action("complete"),
              pause: () => action("pause"),
            })}
        </>
      )}
      {(!queue || queue.status === "completed") &&
        (selection?.items.length ? (
          <>
            <p>
              {t("selectedCount", {
                count: (pendingCreate ?? selection).items.length,
              })}
            </p>
            <Button
              appearance="primary"
              disabled={busy || directBusy || query.isPending || query.isError}
              onClick={() => void start()}
            >
              {t(pendingCreate ? "retryCreate" : "start")}
            </Button>
          </>
        ) : (
          <p>{t("empty")}</p>
        ))}
    </Card>
  );
}
