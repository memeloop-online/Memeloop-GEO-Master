import { useState } from "react";
import {
  Button,
  Card,
  Checkbox,
  Field,
  Input,
} from "@fluentui/react-components";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { useAuth } from "../auth/AuthProvider";
import { membershipForTenant } from "../auth/types";
import { ApiError } from "../api/client";
import {
  getProjectSerpSettings,
  saveProjectSerpSetting,
  testProjectSerpSetting,
  type ProjectSerpSetting,
} from "../api/projectSerp";
import "../i18n/projectSerp";

const newSetting: ProjectSerpSetting = {
  source_key: "primary",
  provider: "dataforseo",
  revision: 0,
  enabled: false,
  credentials_present: false,
  active_credential_revision: null,
  protocol_defaults: {
    query: "",
    engine: "google",
    surface: "third_party_api",
    source: "dataforseo",
    source_location_code: "2840",
    country: "US",
    city: null,
    language: "en",
    device: "desktop",
    operating_system: "windows",
    requested_depth: 10,
    max_pages: 1,
    priority: 1,
    login: "unspecified",
    personalization: "unspecified",
    protocol_version: "geo.serp.v1",
    connector_version: "dataforseo.google.organic.standard.v1",
  },
};

function SourceForm({
  initial,
  tenantId,
  projectId,
  canWrite,
  encryptionAvailable,
  onSaved,
}: {
  initial: ProjectSerpSetting;
  tenantId: string;
  projectId: string;
  canWrite: boolean;
  encryptionAvailable: boolean;
  onSaved: () => void;
}) {
  const { t } = useTranslation("projectSerp");
  const [saved, setSaved] = useState(initial);
  const [enabled, setEnabled] = useState(initial.enabled);
  const [country, setCountry] = useState(initial.protocol_defaults.country);
  const [language, setLanguage] = useState(initial.protocol_defaults.language);
  const [location, setLocation] = useState(
    String(initial.protocol_defaults.source_location_code ?? ""),
  );
  const [login, setLogin] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState<"save" | "test" | null>(null);
  const [error, setError] = useState<string>();
  const [notice, setNotice] = useState<string>();
  const [conflict, setConflict] = useState(false);
  const dirty =
    enabled !== saved.enabled ||
    country !== saved.protocol_defaults.country ||
    language !== saved.protocol_defaults.language ||
    location !== String(saved.protocol_defaults.source_location_code ?? "") ||
    Boolean(login || password);
  const pairValid = Boolean(login.trim()) === Boolean(password);
  const valid =
    pairValid &&
    Boolean(
      country.trim() &&
      language.trim() &&
      /^\d+$/.test(location) &&
      Number(location) > 0,
    ) &&
    (saved.credentials_present || Boolean(login.trim() && password));
  const disabled = !canWrite || Boolean(busy);
  async function save() {
    if (!canWrite || busy || !valid || conflict || !encryptionAvailable) return;
    setBusy("save");
    setError(undefined);
    setNotice(undefined);
    try {
      const result = await saveProjectSerpSetting(
        tenantId,
        projectId,
        saved.source_key,
        {
          expected_revision: saved.revision,
          enabled,
          protocol_defaults: {
            ...saved.protocol_defaults,
            country: country.trim(),
            language: language.trim(),
            source_location_code: location,
          },
          ...(login.trim() && password
            ? { login: login.trim(), password }
            : {}),
        },
      );
      setSaved(result);
      setEnabled(result.enabled);
      setCountry(result.protocol_defaults.country);
      setLanguage(result.protocol_defaults.language);
      setLocation(String(result.protocol_defaults.source_location_code ?? ""));
      setLogin("");
      setPassword("");
      setNotice("saved");
      onSaved();
    } catch (cause) {
      const stale = cause instanceof ApiError && cause.status === 409;
      setConflict(stale);
      setError(stale ? "conflict" : "saveFailed");
    } finally {
      setBusy(null);
    }
  }
  async function test() {
    if (!canWrite || busy || dirty || !saved.credentials_present || conflict)
      return;
    setBusy("test");
    setError(undefined);
    setNotice(undefined);
    try {
      const result = await testProjectSerpSetting(
        tenantId,
        projectId,
        saved.source_key,
        saved.revision,
      );
      if (
        result.source_key !== saved.source_key ||
        result.revision !== saved.revision
      ) {
        setError("testFailed");
        return;
      }
      if (result.status === "connected") setNotice("testPassed");
      else
        setError(
          result.status === "unavailable" ? "unavailable" : "testFailed",
        );
    } catch (cause) {
      const stale = cause instanceof ApiError && cause.status === 409;
      setConflict(stale);
      setError(stale ? "conflict" : "testFailed");
    } finally {
      setLogin("");
      setPassword("");
      setBusy(null);
    }
  }
  function reset() {
    setEnabled(saved.enabled);
    setCountry(saved.protocol_defaults.country);
    setLanguage(saved.protocol_defaults.language);
    setLocation(String(saved.protocol_defaults.source_location_code ?? ""));
    setLogin("");
    setPassword("");
    setError(undefined);
    setNotice(undefined);
  }
  return (
    <Card
      className="channel-card"
      aria-label={`DataForSEO · ${saved.source_key}`}
    >
      <h2>DataForSEO</h2>
      <p>
        {t(saved.enabled ? "enabledState" : "disabledState")} ·{" "}
        {t(
          saved.credentials_present ? "credentialsSaved" : "credentialsMissing",
        )}
      </p>
      <Checkbox
        label={t("enabled")}
        checked={enabled}
        disabled={disabled}
        onChange={(_, data) => setEnabled(Boolean(data.checked))}
      />
      <p>
        {t("defaults", {
          country: country === "US" ? t("countryUs") : country,
          language: language === "en" ? t("languageEn") : language,
        })}
      </p>
      <details>
        <summary>{t("advanced")}</summary>
        <Field label={t("country")} required>
          <Input
            value={country}
            disabled={disabled}
            onChange={(_, data) => setCountry(data.value)}
          />
        </Field>
        <Field label={t("language")} required>
          <Input
            value={language}
            disabled={disabled}
            onChange={(_, data) => setLanguage(data.value)}
          />
        </Field>
        <Field label={t("location")} hint={t("locationHelp")} required>
          <Input
            value={location}
            disabled={disabled}
            onChange={(_, data) => setLocation(data.value)}
          />
        </Field>
      </details>
      {canWrite && (
        <>
          <Field label={t("login")}>
            <Input
              value={login}
              autoComplete="off"
              disabled={disabled || !encryptionAvailable}
              onChange={(_, data) => setLogin(data.value)}
            />
          </Field>
          <Field label={t("password")}>
            <Input
              type="password"
              autoComplete="new-password"
              value={password}
              disabled={disabled || !encryptionAvailable}
              onChange={(_, data) => setPassword(data.value)}
            />
          </Field>
          <p>{t("credentialsHelp")}</p>
          {!pairValid && <p role="status">{t("pairRequired")}</p>}
          {!encryptionAvailable && (
            <p role="status">{t("storageUnavailable")}</p>
          )}
          {dirty && <p role="status">{t("dirty")}</p>}
          <Button
            disabled={
              disabled || !dirty || !valid || conflict || !encryptionAvailable
            }
            onClick={() => void save()}
          >
            {t(busy === "save" ? "saving" : "save")}
          </Button>
          <Button disabled={disabled || !dirty} onClick={reset}>
            {t("reset")}
          </Button>
          <Button
            disabled={
              disabled || dirty || conflict || !saved.credentials_present
            }
            onClick={() => void test()}
          >
            {t(busy === "test" ? "testing" : "test")}
          </Button>
          <p>{t(dirty ? "saveFirst" : "testHelp")}</p>
        </>
      )}
      {error && <p role="alert">{t(error)}</p>}
      {notice && <p role="status">{t(notice)}</p>}
    </Card>
  );
}

export function ProjectSerpSettingsPanel({
  tenantId,
  projectId,
}: {
  tenantId: string;
  projectId: string;
}) {
  const { session } = useAuth();
  const { t } = useTranslation("projectSerp");
  const client = useQueryClient();
  const canWrite =
    membershipForTenant(session, tenantId)?.role === "tenant_admin";
  const query = useQuery({
    queryKey: [
      "project-serp-settings",
      session?.user.id,
      session?.operator.id,
      tenantId,
      projectId,
    ],
    queryFn: () => getProjectSerpSettings(tenantId, projectId),
    enabled: Boolean(session),
    retry: false,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
    gcTime: 0,
  });
  const [reload, setReload] = useState(0);
  async function refresh() {
    const result = await query.refetch();
    if (result.isSuccess) setReload((value) => value + 1);
  }
  return (
    <section className="channel-stack" aria-label={t("title")}>
      <h2>{t("title")}</h2>
      <p>{t("description")}</p>
      {!canWrite && <p>{t("readOnly")}</p>}
      <Button disabled={query.isFetching} onClick={() => void refresh()}>
        {t("reload")}
      </Button>
      {query.isPending ? (
        <p role="status">{t("loading")}</p>
      ) : query.isError ? (
        <p role="alert">{t("loadFailed")}</p>
      ) : (
        (query.data.items.length
          ? query.data.items
          : canWrite
            ? [newSetting]
            : []
        ).map((setting) => (
          <SourceForm
            key={`${session?.user.id}/${session?.operator.id}/${tenantId}/${projectId}/${setting.source_key}/${reload}`}
            initial={setting}
            tenantId={tenantId}
            projectId={projectId}
            canWrite={canWrite}
            encryptionAvailable={query.data.encryption_available}
            onSaved={() => {
              void client.invalidateQueries({
                queryKey: ["serp-capabilities"],
              });
            }}
          />
        ))
      )}
      {query.data && !query.data.items.length && !canWrite && (
        <p>{t("empty")}</p>
      )}
    </section>
  );
}
