import { useEffect, useRef, useState } from "react";
import {
  Button,
  Card,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
  Select,
  Spinner,
  Textarea,
} from "@fluentui/react-components";
import { useTranslation } from "react-i18next";
import Papa from "papaparse";
import "../i18n/accountImports";
import { ApiError, createIdempotencyKey } from "../api/client";
import {
  createAccountImport,
  getAccountImport,
  retryAccountImportRow,
  type AccountImportRequest,
  type AccountImportResult,
  type AccountImportRow,
} from "../api/accountImports";
import type {
  ChannelPlatform,
  ChannelPlatformId,
  PoolGroup,
} from "../api/channels";

const maxBytes = 1024 * 1024;
const byteLength = (value: string) => new TextEncoder().encode(value).length;
type RecoveryManifest = { import_key: string; row_keys: string[] };

export function OperatorAccountImport({
  platforms,
  groups,
  onChanged,
  onLogin,
  storageScope,
  loginDisabled = false,
}: {
  platforms: ChannelPlatform[];
  groups: PoolGroup[];
  onChanged: () => Promise<unknown>;
  onLogin: (accountId: string) => void;
  storageScope: string;
  loginDisabled?: boolean;
}) {
  const { t } = useTranslation("accountImports");
  const [platform, setPlatform] = useState<ChannelPlatformId>(
    platforms[0]?.id ?? "zhihu",
  );
  const [group, setGroup] = useState("");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [alias, setAlias] = useState("");
  const [csv, setCsv] = useState("");
  const [count, setCount] = useState(0);
  const [busy, setBusy] = useState(false);
  const [submittedBatch, setSubmittedBatch] = useState(false);
  const [error, setError] = useState("");
  const [reference, setReference] = useState("");
  const [result, setResult] = useState<AccountImportResult>();
  const [recovery, setRecovery] = useState<RecoveryManifest>();
  const [legacyResult, setLegacyResult] = useState(false);
  // Secrets never enter a query cache, browser storage, URL, or result preview.
  const rows = useRef<AccountImportRow[]>([]);
  const pending = useRef<AccountImportRequest | undefined>(undefined);
  const recoveryRef = useRef<RecoveryManifest | undefined>(undefined);
  const mounted = useRef(true);
  const locked = useRef(false);
  const fileInput = useRef<HTMLInputElement>(null);
  const storageKey = `account-import-reference:${storageScope}`;
  const recoveryKey = `${storageKey}:pending`;
  const completionKey = `${storageKey}:complete`;
  const editingDisabled = busy || submittedBatch;
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      rows.current = [];
      pending.current = undefined;
    };
  }, []);
  useEffect(() => {
    try {
      const raw = sessionStorage.getItem(recoveryKey);
      const manifest = raw ? (JSON.parse(raw) as RecoveryManifest) : undefined;
      if (
        manifest &&
        typeof manifest.import_key === "string" &&
        Array.isArray(manifest.row_keys) &&
        manifest.row_keys.length > 0 &&
        manifest.row_keys.length <= 100 &&
        manifest.row_keys.every(
          (key) =>
            typeof key === "string" && key.length > 0 && key.length <= 128,
        ) &&
        new Set(manifest.row_keys).size === manifest.row_keys.length
      ) {
        recoveryRef.current = manifest;
        setRecovery(manifest);
      }
      const saved =
        recoveryRef.current?.import_key ?? sessionStorage.getItem(storageKey);
      if (saved) {
        setReference(saved);
        void operation(() => getAccountImport(saved));
      }
    } catch {
      // Storage may be unavailable. Manual reference retrieval remains usable.
    }
    // The parent keys the component by authenticated scope.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [storageKey]);

  function saveReference(key: string) {
    setReference(key);
    try {
      sessionStorage.setItem(
        storageKey,
        recoveryRef.current?.import_key ?? key,
      );
    } catch {
      /* optional nonsecret reference */
    }
  }

  function parseError(cause: unknown) {
    const key =
      cause instanceof Error &&
      ["fieldLimits", "recoveryCount"].includes(cause.message)
        ? cause.message
        : "invalid";
    setError(t(key, { count: recoveryRef.current?.row_keys.length }));
  }

  function clear() {
    rows.current = [];
    pending.current = undefined;
    setSubmittedBatch(false);
    setCount(0);
    setUsername("");
    setPassword("");
    setAlias("");
    setCsv("");
    if (fileInput.current) fileInput.current.value = "";
  }

  function validationError(next: AccountImportRow[]) {
    if (
      next.some(
        (row) =>
          byteLength(row.username ?? "") > 512 ||
          byteLength(row.password ?? "") > 4096 ||
          byteLength(row.display_alias ?? "") > 160 ||
          byteLength(row.platform ?? "") > 64,
      )
    )
      return "fieldLimits";
    return next.length > 0 &&
      next.length <= 100 &&
      byteLength(
        JSON.stringify({ import_key: createIdempotencyKey(), rows: next }),
      ) <= maxBytes &&
      next.every(
        (row) =>
          row.username?.trim() &&
          row.password &&
          platforms.some(
            (entry) => entry.id === row.platform && entry.login_entry_available,
          ) &&
          (!row.group_id ||
            groups.some((entry) => entry.group_id === row.group_id)),
      )
      ? undefined
      : "invalid";
  }

  function prepareCsv(text: string) {
    if (submittedBatch) return;
    clear();
    if (byteLength(text) > maxBytes) throw new Error("invalid");
    const parsed = Papa.parse<Record<string, string>>(text, {
      header: true,
      skipEmptyLines: "greedy",
      delimiter: ",",
      transformHeader: (header) => header.replace(/^\uFEFF/, "").trim(),
    });
    const fields = parsed.meta.fields ?? [];
    const allowed = [
      "username",
      "password",
      "platform",
      "display_alias",
      "group_id",
    ];
    if (
      parsed.errors.length ||
      !fields.includes("username") ||
      !fields.includes("password") ||
      fields.some((field) => !allowed.includes(field)) ||
      new Set(fields).size !== fields.length
    ) {
      throw new Error("invalid");
    }
    const next = parsed.data.map((row) => ({
      row_key: createIdempotencyKey(),
      platform: (row.platform?.trim() || platform) as ChannelPlatformId,
      username: row.username,
      password: row.password,
      ...(row.display_alias?.trim()
        ? { display_alias: row.display_alias.trim() }
        : {}),
      ...(row.group_id?.trim() || group
        ? { group_id: row.group_id?.trim() || group }
        : {}),
    }));
    const invalid = validationError(next);
    if (invalid) throw new Error(invalid);
    if (
      recoveryRef.current &&
      next.length !== recoveryRef.current.row_keys.length
    )
      throw new Error("recoveryCount");
    rows.current = next;
    pending.current = undefined;
    setCount(next.length);
    setError("");
  }

  async function operation(action: () => Promise<AccountImportResult>) {
    if (locked.current) return;
    locked.current = true;
    setBusy(true);
    setError("");
    try {
      const next = await action();
      if (!mounted.current) return;
      const manifest = recoveryRef.current;
      let knownComplete = false;
      try {
        knownComplete =
          sessionStorage.getItem(completionKey) === next.import_key;
      } catch {
        /* unknown completeness */
      }
      setLegacyResult(
        manifest?.import_key !== next.import_key && !knownComplete,
      );
      if (
        manifest?.import_key === next.import_key &&
        manifest.row_keys.every((key) =>
          next.rows.some((saved) => saved.row_key === key),
        )
      ) {
        clear();
        recoveryRef.current = undefined;
        setRecovery(undefined);
        try {
          sessionStorage.setItem(completionKey, next.import_key);
          sessionStorage.removeItem(recoveryKey);
        } catch {
          /* nonsecret recovery only */
        }
      }
      setResult(next);
      saveReference(next.import_key);
      // A list refresh failure must not turn an accepted import into a failed submission.
      await onChanged().catch(() => {});
    } catch (cause) {
      if (mounted.current)
        setError(
          t(
            cause instanceof ApiError && cause.status === 409
              ? "recoveryChanged"
              : "failed",
          ),
        );
    } finally {
      locked.current = false;
      if (mounted.current) setBusy(false);
    }
  }

  function submit() {
    if (!rows.current.length || locked.current) return;
    const prior = recoveryRef.current;
    if (prior && rows.current.length !== prior.row_keys.length) {
      setError(t("recoveryCount", { count: prior.row_keys.length }));
      return;
    }
    pending.current ??= {
      import_key: prior?.import_key ?? createIdempotencyKey(),
      rows: rows.current.map((row, index) => ({
        ...row,
        row_key: prior?.row_keys[index] ?? row.row_key,
      })),
    };
    const manifest = {
      import_key: pending.current.import_key,
      row_keys: pending.current.rows.map((row) => row.row_key),
    };
    try {
      sessionStorage.setItem(recoveryKey, JSON.stringify(manifest));
    } catch {
      setError(t("recoveryStorage"));
      return;
    }
    recoveryRef.current = manifest;
    setRecovery(manifest);
    setSubmittedBatch(true);
    saveReference(pending.current.import_key);
    void operation(() => createAccountImport(pending.current!));
  }

  return (
    <Card className="channel-card" aria-label={t("title")}>
      <h2>{t("title")}</h2>
      <p>{t("help")}</p>
      <p>{t("format")}</p>
      <div className="channel-form">
        <Field label={t("platform")}>
          <Select
            value={platform}
            disabled={editingDisabled}
            onChange={(_, data) => setPlatform(data.value as ChannelPlatformId)}
          >
            {platforms
              .filter((entry) => entry.login_entry_available)
              .map((entry) => (
                <option key={entry.id} value={entry.id}>
                  {entry.label}
                </option>
              ))}
          </Select>
        </Field>
        <Field label={t("group")}>
          <Select
            value={group}
            disabled={editingDisabled}
            onChange={(_, data) => setGroup(data.value)}
          >
            <option value="">{t("ungrouped")}</option>
            {groups.map((entry) => (
              <option key={entry.group_id} value={entry.group_id}>
                {entry.name}
              </option>
            ))}
          </Select>
        </Field>
        <Field label={t("file")}>
          <input
            ref={fileInput}
            aria-label={t("file")}
            type="file"
            accept=".csv,text/csv"
            disabled={editingDisabled}
            onChange={async (event) => {
              const file = event.target.files?.[0];
              event.target.value = "";
              if (!file || locked.current || submittedBatch) return;
              clear();
              locked.current = true;
              setBusy(true);
              try {
                if (file.size > maxBytes) throw new Error("invalid");
                const text = await file.text();
                if (mounted.current) prepareCsv(text);
              } catch (cause) {
                if (mounted.current) parseError(cause);
              } finally {
                locked.current = false;
                if (mounted.current) setBusy(false);
              }
            }}
          />
        </Field>
        <details>
          <summary>{t("paste")}</summary>
          <Field label={t("paste")} hint={t("pasteHelp")}>
            <Textarea
              value={csv}
              autoComplete="off"
              spellCheck={false}
              disabled={editingDisabled}
              onChange={(_, data) => {
                if (submittedBatch || locked.current) return;
                clear();
                if (byteLength(data.value) > maxBytes) {
                  setCsv("");
                  setError(t("invalid"));
                  return;
                }
                setCsv(data.value);
              }}
            />
          </Field>
          <Button
            disabled={editingDisabled || !csv}
            onClick={() => {
              try {
                prepareCsv(csv);
              } catch (cause) {
                parseError(cause);
              } finally {
                setCsv("");
              }
            }}
          >
            {t("prepare")}
          </Button>
        </details>
        <Field label={t("username")}>
          <Input
            autoComplete="off"
            value={username}
            disabled={editingDisabled}
            onChange={(_, data) => setUsername(data.value)}
          />
        </Field>
        <Field label={t("password")}>
          <Input
            type="password"
            autoComplete="new-password"
            value={password}
            disabled={editingDisabled}
            onChange={(_, data) => setPassword(data.value)}
          />
        </Field>
        <Field label={t("alias")}>
          <Input
            value={alias}
            disabled={editingDisabled}
            onChange={(_, data) => setAlias(data.value)}
          />
        </Field>
        <Button
          disabled={
            editingDisabled || !username.trim() || !password || count >= 100
          }
          onClick={() => {
            const next = [
              ...rows.current,
              {
                row_key: createIdempotencyKey(),
                platform,
                username,
                password,
                ...(alias.trim() ? { display_alias: alias.trim() } : {}),
                ...(group ? { group_id: group } : {}),
              },
            ];
            const invalid = validationError(next);
            if (invalid) {
              setError(t(invalid));
              return;
            }
            rows.current = next;
            pending.current = undefined;
            setCount(next.length);
            setUsername("");
            setPassword("");
            setAlias("");
            setError("");
          }}
        >
          {t("add")}
        </Button>
      </div>
      <p role="status">{t("count", { count })}</p>
      <div className="channel-row">
        <Button appearance="primary" disabled={busy || !count} onClick={submit}>
          {t("submit")}
        </Button>
        <Button
          disabled={busy}
          onClick={() => {
            if (recoveryRef.current && !window.confirm(t("clearConfirm")))
              return;
            clear();
          }}
        >
          {t("clear")}
        </Button>
      </div>
      {submittedBatch && <p>{t("pendingHelp")}</p>}
      {recovery && (
        <p role="status">
          {t("recoveryHelp", {
            saved:
              result?.import_key === recovery.import_key
                ? result.rows.filter((row) =>
                    recovery.row_keys.includes(row.row_key),
                  ).length
                : 0,
            count: recovery.row_keys.length,
          })}
        </p>
      )}
      {legacyResult && <p>{t("legacyHelp")}</p>}
      <details>
        <summary>{t("reference")}</summary>
        <Field label={t("reference")} hint={t("referenceHelp")}>
          <Input
            value={reference}
            disabled={busy}
            onChange={(_, data) => setReference(data.value)}
          />
        </Field>
        <Button
          disabled={busy || !reference.trim()}
          onClick={() =>
            void operation(() => getAccountImport(reference.trim()))
          }
        >
          {t("load")}
        </Button>
      </details>
      {busy && <Spinner size="tiny" label={t("busy")} />}
      {error && (
        <MessageBar intent="error">
          <MessageBarBody>{error}</MessageBarBody>
        </MessageBar>
      )}
      {result && (
        <section aria-label={t("results")}>
          <h3>{t("results")}</h3>
          <p>
            {t("reference")}: {result.import_key}
          </p>
          <ul className="channel-accounts">
            {result.rows.map((row) => (
              <li key={row.row_key}>
                <p>
                  {t("row", { number: row.ordinal + 1 })} ·{" "}
                  {row.display_alias || row.masked_username}
                </p>
                <p>
                  {row.status === "needs_login"
                    ? t("needs_login")
                    : t(`errors.${row.error_code}`, {
                        defaultValue: t("rowFailed"),
                      })}
                </p>
                {row.status === "failed" ? (
                  <Button
                    disabled={busy}
                    onClick={() =>
                      void operation(() =>
                        retryAccountImportRow(result.import_key, row.row_key),
                      )
                    }
                  >
                    {t("retry")}
                  </Button>
                ) : (
                  row.account_id && (
                    <Button
                      disabled={busy || loginDisabled}
                      onClick={() => onLogin(row.account_id!)}
                    >
                      {t("login")}
                    </Button>
                  )
                )}
              </li>
            ))}
          </ul>
        </section>
      )}
    </Card>
  );
}
