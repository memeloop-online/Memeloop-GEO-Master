import { useEffect, useRef, useState } from "react";
import {
  Button,
  Card,
  Checkbox,
  Field,
  Input,
  Select,
} from "@fluentui/react-components";
import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { Link, useSearchParams } from "react-router-dom";
import { useAuth } from "../auth/AuthProvider";
import { createIdempotencyKey } from "../api/client";
import * as api from "../api/serp";
import { safeOriginalPublicUrl } from "./PublicationLookupPanel";
import "../i18n/serp";
import "../i18n/projectSerp";

const pending = new Set<api.SerpState>([
  "queued",
  "claimed",
  "sending",
  "awaiting_result",
  "unknown",
]);
const resultKinds = new Set([
  "organic",
  "advertisement",
  "featured_snippet",
  "maps",
  "ai_overview",
  "other",
]);

export function SerpMeasurementPanel(props: {
  tenantId: string;
  projectId: string;
}) {
  return <Panel key={`${props.tenantId}:${props.projectId}`} {...props} />;
}

function Panel({
  tenantId,
  projectId,
}: {
  tenantId: string;
  projectId: string;
}) {
  const { t, i18n } = useTranslation("serp");
  const { session } = useAuth();
  const scope = [session?.user.id, session?.operator.id, tenantId, projectId];
  const canWrite = Boolean(
    session?.memberships.some(
      (member) =>
        member.tenant_id === tenantId &&
        ["tenant_admin", "member"].includes(member.role),
    ),
  );
  const [query, setQuery] = useState("");
  const [source, setSource] = useState("");
  const [mode, setMode] = useState<"none" | "host" | "url">("none");
  const [targetText, setTargetText] = useState("");
  const [subdomains, setSubdomains] = useState(true);
  const [params, setParams] = useSearchParams();
  const selected = params.get("searchRecord") || undefined;
  const setSelected = (id?: string) =>
    setParams((previous) => {
      const next = new URLSearchParams(previous);
      if (id) next.set("searchRecord", id);
      else next.delete("searchRecord");
      return next;
    });
  const [rawId, setRawId] = useState<string>();
  useEffect(() => setRawId(undefined), [selected]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<
    "createError" | "invalidTarget" | "cancelError"
  >();
  const intent = useRef<
    { signature: string; request: api.CreateSerpMeasurement } | undefined
  >(undefined);
  const capabilities = useQuery({
    queryKey: ["serp-capabilities", ...scope],
    queryFn: () => api.getSerpCapabilities(tenantId, projectId),
    enabled: Boolean(session),
    retry: false,
  });
  const history = useInfiniteQuery({
    queryKey: ["serp-history", ...scope],
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) =>
      api.listSerpMeasurements(tenantId, projectId, pageParam),
    getNextPageParam: (last) => last.next_after ?? undefined,
    enabled: Boolean(session),
    retry: false,
    refetchInterval: (result) =>
      result.state.data?.pages.some((page) =>
        page.items.some((item) => pending.has(item.state)),
      )
        ? 5000
        : false,
  });
  const detail = useInfiniteQuery({
    queryKey: ["serp-detail", ...scope, selected],
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) =>
      api.getSerpMeasurement(tenantId, projectId, selected!, pageParam),
    getNextPageParam: (last) => last.next_after ?? undefined,
    enabled: Boolean(session && selected),
    retry: false,
    refetchInterval: (result) =>
      pending.has(
        result.state.data?.pages[0]?.measurement.state as api.SerpState,
      )
        ? 5000
        : false,
  });
  const sources = useInfiniteQuery({
    queryKey: [
      "serp-sources",
      ...scope,
      selected,
      detail.data?.pages[0]?.measurement.state,
      detail.data?.pages[0]?.observations[0]?.raw_evidence_id,
    ],
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) =>
      api.listSerpSources(tenantId, projectId, selected!, pageParam),
    getNextPageParam: (last) => last.next_after ?? undefined,
    enabled: Boolean(session && selected),
    retry: false,
    refetchInterval: () =>
      pending.has(detail.data?.pages[0]?.measurement.state as api.SerpState)
        ? 5000
        : false,
  });
  const raw = useQuery({
    queryKey: ["serp-raw", ...scope, selected, rawId],
    queryFn: () => api.getSerpRaw(tenantId, projectId, selected!, rawId!),
    enabled: Boolean(session && selected && rawId),
    retry: false,
  });
  const selectedSource = source || capabilities.data?.[0]?.source_key || "";
  const at = (time: string) =>
    new Intl.DateTimeFormat(i18n.language, {
      dateStyle: "medium",
      timeStyle: "short",
    }).format(new Date(time));
  const status = (state: api.SerpState) => t(`states.${state}`);
  const current = detail.data?.pages[0];
  const measurements = history.data?.pages.flatMap((page) => page.items) ?? [];
  const observations =
    detail.data?.pages.flatMap((page) => page.observations) ?? [];
  async function submit() {
    if (!canWrite || busy || !query.trim() || !selectedSource) return;
    let target: api.SerpTarget | undefined;
    try {
      if (mode === "url") {
        if (!safeOriginalPublicUrl(targetText)) throw new Error();
        target = { kind: "url", url: targetText };
      } else if (mode === "host") {
        if (
          !targetText ||
          /[\s/?#@:\\]/.test(targetText) ||
          targetText.endsWith(".")
        )
          throw new Error();
        const host = new URL(`https://${targetText}`).hostname;
        target = { kind: "host", host, include_subdomains: subdomains };
      }
    } catch {
      setError("invalidTarget");
      return;
    }
    const signature = JSON.stringify([query, target, selectedSource]);
    if (intent.current?.signature !== signature) {
      intent.current = {
        signature,
        request: {
          query,
          target,
          source_key: selectedSource,
          idempotency_key: createIdempotencyKey(),
          scheduled_at: new Date().toISOString(),
        },
      };
    }
    setBusy(true);
    setError(undefined);
    try {
      const result = await api.createSerpMeasurement(
        tenantId,
        projectId,
        intent.current.request,
      );
      setSelected(result.measurement_id);
      setRawId(undefined);
      intent.current = undefined;
      await history.refetch();
    } catch {
      setError("createError");
      void history.refetch();
    } finally {
      setBusy(false);
    }
  }
  async function cancel() {
    if (!selected || !canWrite || busy) return;
    setBusy(true);
    setError(undefined);
    try {
      await api.cancelSerpMeasurement(tenantId, projectId, selected);
      await Promise.all([detail.refetch(), history.refetch()]);
    } catch {
      setError("cancelError");
    } finally {
      setBusy(false);
    }
  }
  const refresh = () => {
    void capabilities.refetch();
    void history.refetch();
    if (selected) {
      void detail.refetch();
      void sources.refetch();
    }
  };
  return (
    <section aria-label={t("title")} className="report-section">
      <h2>{t("title")}</h2>
      <p>{t("description")}</p>
      <Button onClick={refresh}>{t("refresh")}</Button>
      {capabilities.isError ? (
        <p role="alert">{t("capabilityError")}</p>
      ) : capabilities.isPending ? (
        <p role="status">{t("loading")}</p>
      ) : !capabilities.data?.length ? (
        <p role="status">
          {t("unavailable")}{" "}
          <Link
            to={`/app/${encodeURIComponent(tenantId)}/${encodeURIComponent(projectId)}/settings?tab=search`}
          >
            {t("goSettings", { ns: "projectSerp" })}
          </Link>
        </p>
      ) : null}
      {!canWrite && <p>{t("readOnly")}</p>}
      <form
        onSubmit={(event) => {
          event.preventDefault();
          void submit();
        }}
        style={{ display: "grid", gap: 12, maxWidth: 720 }}
      >
        <Field label={t("query")} required>
          <Input
            value={query}
            maxLength={700}
            disabled={busy || !canWrite}
            onChange={(_, data) => setQuery(data.value)}
          />
        </Field>
        <Field label={t("source")}>
          <Select
            value={selectedSource}
            disabled={busy || !capabilities.data?.length || !canWrite}
            onChange={(_, data) => setSource(data.value)}
          >
            {!capabilities.data?.length && (
              <option value="">{t("unavailable")}</option>
            )}
            {capabilities.data?.map((item) => (
              <option key={item.source_key} value={item.source_key}>
                {item.source_key} · {item.protocol_defaults.country} ·{" "}
                {item.protocol_defaults.language}
              </option>
            ))}
          </Select>
        </Field>
        <Field label={t("targetMode")}>
          <Select
            value={mode}
            disabled={busy || !canWrite}
            onChange={(_, data) => setMode(data.value as typeof mode)}
          >
            <option value="none">{t("none")}</option>
            <option value="host">{t("host")}</option>
            <option value="url">{t("url")}</option>
          </Select>
        </Field>
        {mode !== "none" && (
          <Field label={t("target")} required>
            <Input
              value={targetText}
              disabled={busy || !canWrite}
              onChange={(_, data) => setTargetText(data.value)}
            />
          </Field>
        )}
        {mode === "host" && (
          <Checkbox
            checked={subdomains}
            disabled={busy || !canWrite}
            label={t("subdomains")}
            onChange={(_, data) => setSubdomains(Boolean(data.checked))}
          />
        )}
        <Button
          type="submit"
          appearance="primary"
          disabled={
            busy ||
            !canWrite ||
            !query.trim() ||
            !capabilities.data?.some(
              (item) => item.source_key === selectedSource,
            )
          }
        >
          {t(busy ? "creating" : "create")}
        </Button>
      </form>
      {error && <p role="alert">{t(error)}</p>}
      <h3>{t("history")}</h3>
      {history.isError && <p role="alert">{t("readError")}</p>}
      {history.isPending ? (
        <p role="status">{t("loading")}</p>
      ) : !measurements.length && !history.isError ? (
        <p>{t("empty")}</p>
      ) : null}
      <ul>
        {measurements.map((item) => (
          <li key={item.measurement_id}>
            <Button
              onClick={() => {
                setSelected(item.measurement_id);
                setRawId(undefined);
              }}
            >
              {item.protocol.query}
            </Button>{" "}
            — {status(item.state)}
          </li>
        ))}
      </ul>
      {history.hasNextPage && (
        <Button
          disabled={history.isFetchingNextPage}
          onClick={() => void history.fetchNextPage()}
        >
          {t("more")}
        </Button>
      )}
      {selected && (
        <section aria-label={t("detail")}>
          <h3>{t("detail")}</h3>
          <Button
            onClick={() => {
              setSelected(undefined);
              setRawId(undefined);
            }}
          >
            {t("close")}
          </Button>
          {detail.isPending && <p role="status">{t("loading")}</p>}
          {detail.isError && <p role="alert">{t("readError")}</p>}
          {current && (
            <>
              <h4>{current.measurement.protocol.query}</h4>
              <p>{status(current.measurement.state)}</p>
              <p>{t("requested", current.measurement.protocol)}</p>
              <p>
                {t("scheduled", { time: at(current.measurement.scheduled_at) })}
              </p>
              {current.execution?.next_poll_at && (
                <p>
                  {t("nextPoll", { time: at(current.execution.next_poll_at) })}
                </p>
              )}
              {canWrite && pending.has(current.measurement.state) && (
                <Button disabled={busy} onClick={() => void cancel()}>
                  {t("cancel")}
                </Button>
              )}
              {!observations.length && <p>{t("noObservation")}</p>}
              {observations.map((observation) => (
                <Card
                  key={observation.observation_id}
                  style={{ marginTop: 12 }}
                >
                  <h4>
                    {t("observation", {
                      time: at(
                        observation.provider_observed_at ??
                          observation.received_at,
                      ),
                    })}
                  </h4>
                  <p>
                    {t(`statuses.${observation.status}`, {
                      defaultValue: t("statuses.missing"),
                    })}
                  </p>
                  <p>{t("received", { time: at(observation.received_at) })}</p>
                  <p>
                    {t("depth", {
                      observed: observation.coverage.observed_organic_depth,
                      requested: observation.coverage.requested_depth,
                    })}
                  </p>
                  {(observation.coverage.completion === "partial" ||
                    observation.coverage.truncated) && <p>{t("partial")}</p>}
                  {observation.source_limitations.length > 0 && (
                    <ul>
                      {observation.source_limitations.map((limitation) => (
                        <li key={limitation}>
                          {t(`limitations.${limitation}`, {
                            defaultValue: t("conditionsUnknown"),
                          })}
                        </li>
                      ))}
                    </ul>
                  )}
                  <ol>
                    {observation.results.map((result) => {
                      const href = safeOriginalPublicUrl(result.raw_url);
                      return (
                        <li
                          key={`${result.position}:${result.locator}`}
                          style={{ marginBottom: 12, overflowWrap: "anywhere" }}
                        >
                          {href ? (
                            <a href={href} target="_blank" rel="noreferrer">
                              {result.title || result.raw_url}
                            </a>
                          ) : (
                            <span>{result.title || result.raw_url || "—"}</span>
                          )}
                          <p>
                            {t(
                              `kinds.${resultKinds.has(result.kind) ? result.kind : "other"}`,
                            )}{" "}
                            ·{" "}
                            {result.kind === "organic"
                              ? result.organic_rank != null
                                ? `${t("rank")}: ${result.organic_rank}`
                                : t("rankUnknown")
                              : t("notOrganic")}
                            {result.absolute_position != null &&
                              ` · ${t("absolute")}: ${result.absolute_position}`}
                          </p>
                          <code>{result.locator}</code>
                        </li>
                      );
                    })}
                  </ol>
                  <Button onClick={() => setRawId(observation.raw_evidence_id)}>
                    {t("showRaw")}
                  </Button>
                </Card>
              ))}
              {detail.hasNextPage && (
                <Button
                  disabled={detail.isFetchingNextPage}
                  onClick={() => void detail.fetchNextPage()}
                >
                  {t("more")}
                </Button>
              )}
            </>
          )}
          <h4>{t("evidence")}</h4>
          {sources.isError && <p role="alert">{t("readError")}</p>}
          {sources.data?.pages.every((page) => !page.items.length) && (
            <p>{t("sourceEmpty")}</p>
          )}
          <ul>
            {sources.data?.pages
              .flatMap((page) => page.items)
              .map((item) => (
                <li key={item.evidence_id}>
                  {at(item.captured_at)} ·{" "}
                  {t(item.body_complete ? "complete" : "incomplete")} ·{" "}
                  {t("bytes", { count: item.body_bytes })}
                  <Button onClick={() => setRawId(item.evidence_id)}>
                    {t("showRaw")}
                  </Button>
                </li>
              ))}
          </ul>
          {sources.hasNextPage && (
            <Button
              disabled={sources.isFetchingNextPage}
              onClick={() => void sources.fetchNextPage()}
            >
              {t("more")}
            </Button>
          )}
          {rawId && (
            <section aria-label={t("raw")}>
              <h4>{t("raw")}</h4>
              {raw.isPending && <p role="status">{t("loading")}</p>}
              {raw.isError && (
                <>
                  <p role="alert">{t("rawError")}</p>
                  <Button onClick={() => void raw.refetch()}>
                    {t("refresh")}
                  </Button>
                </>
              )}
              {raw.data && (
                <>
                  <code style={{ overflowWrap: "anywhere" }}>
                    {raw.data.evidence.response_sha256}
                  </code>
                  <pre
                    style={{
                      whiteSpace: "pre-wrap",
                      overflowWrap: "anywhere",
                      maxHeight: 360,
                      overflow: "auto",
                    }}
                  >
                    {new TextDecoder().decode(
                      new Uint8Array(raw.data.evidence.body),
                    )}
                  </pre>
                </>
              )}
            </section>
          )}
        </section>
      )}
    </section>
  );
}
