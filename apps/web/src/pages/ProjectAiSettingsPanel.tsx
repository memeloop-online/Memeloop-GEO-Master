import { useEffect, useState } from "react";
import {
  Button,
  Card,
  Checkbox,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
  Select,
} from "@fluentui/react-components";
import { useTranslation } from "react-i18next";
import { useAuth } from "../auth/AuthProvider";
import { membershipForTenant } from "../auth/types";
import { ApiError } from "../api/client";
import {
  saveProjectAiSetting,
  discoverProjectAiModels,
  testProjectAiSetting,
  useProjectAiSettings,
  type ProjectAiSetting,
} from "../api/projectAi";
import { ErrorState, LoadingState } from "../components/AsyncState";
import "../i18n/projectAi";

function AiSettingForm({
  setting,
  tenantId,
  projectId,
  canWrite,
  onSaved,
}: {
  setting: ProjectAiSetting;
  tenantId: string;
  projectId: string;
  canWrite: boolean;
  onSaved: () => Promise<boolean>;
}) {
  const { t } = useTranslation();
  const [mode, setMode] = useState(setting.mode);
  const [model, setModel] = useState(setting.model ?? "");
  const [baseUrl, setBaseUrl] = useState(setting.base_url ?? "");
  const [apiKey, setApiKey] = useState("");
  const [preferBrowser, setPreferBrowser] = useState(
    setting.prefer_connected_account ?? true,
  );
  const [busy, setBusy] = useState<"save" | "test" | "discover" | null>(null);
  const [models, setModels] = useState<string[]>([]);
  const [discoveryMessage, setDiscoveryMessage] = useState("");
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [conflict, setConflict] = useState(false);
  const observation = setting.usage === "observation_analysis";
  const dirty =
    mode !== setting.mode ||
    model !== (setting.model ?? "") ||
    baseUrl !== (setting.base_url ?? "") ||
    Boolean(apiKey) ||
    (observation &&
      preferBrowser !== (setting.prefer_connected_account ?? true));
  const valid =
    mode === "inherit" ||
    Boolean(
      model.trim() && baseUrl.trim() && (apiKey.trim() || setting.key_present),
    );
  const disabled = !canWrite || busy !== null;
  const title = t(
    observation ? "projectAi.observation" : "projectAi.inference",
  );
  useEffect(() => {
    setMode(setting.mode);
    setModel(setting.model ?? "");
    setBaseUrl(setting.base_url ?? "");
    setApiKey("");
    setPreferBrowser(setting.prefer_connected_account ?? true);
    setConflict(false);
    setModels([]);
    setDiscoveryMessage("");
  }, [setting]);

  function reset() {
    setMode(setting.mode);
    setModel(setting.model ?? "");
    setBaseUrl(setting.base_url ?? "");
    setApiKey("");
    setPreferBrowser(setting.prefer_connected_account ?? true);
    setError("");
    setNotice("");
  }
  async function save() {
    setBusy("save");
    setError("");
    setNotice("");
    try {
      await saveProjectAiSetting(tenantId, projectId, setting.usage, {
        expected_revision: setting.revision,
        mode,
        ...(mode === "custom"
          ? {
              model: model.trim(),
              base_url: baseUrl.trim(),
              ...(apiKey.trim() ? { api_key: apiKey.trim() } : {}),
            }
          : {}),
        ...(observation ? { prefer_connected_account: preferBrowser } : {}),
      });
      setApiKey("");
      if (!(await onSaved())) {
        setConflict(true);
        setError(t("projectAi.readbackFailed"));
        return;
      }
      setNotice(t("projectAi.saved"));
    } catch (cause) {
      const stale = cause instanceof ApiError && cause.status === 409;
      setConflict(stale);
      // Never render raw provider errors: they may echo submitted credentials.
      setError(t(stale ? "projectAi.conflict" : "projectAi.saveFailed"));
    } finally {
      setBusy(null);
    }
  }
  async function test() {
    setBusy("test");
    setError("");
    setNotice("");
    try {
      const result = await testProjectAiSetting(
        tenantId,
        projectId,
        setting.usage,
        setting.revision,
      );
      if (result.success) setNotice(t("projectAi.testPassed"));
      else setError(t("projectAi.testFailed"));
    } catch {
      setError(t("projectAi.testFailed"));
    } finally {
      setBusy(null);
    }
  }
  async function discover() {
    setBusy("discover");
    setDiscoveryMessage("");
    setModels([]);
    try {
      const result = await discoverProjectAiModels(
        tenantId,
        projectId,
        setting.usage,
        setting.revision,
      );
      const ids = [...new Set(result.items.map((item) => item.id))];
      setModels(ids);
      if (!ids.length) setDiscoveryMessage(t("projectAi.noModels"));
    } catch {
      setDiscoveryMessage(t("projectAi.discoveryFailed"));
    } finally {
      setBusy(null);
    }
  }
  return (
    <Card className="channel-card" aria-label={title}>
      <h2>{title}</h2>
      <p>
        {t(
          observation ? "projectAi.observationHelp" : "projectAi.inferenceHelp",
        )}
      </p>
      {mode === "inherit" && (
        <p>
          {setting.effective.configured
            ? setting.effective.model
              ? t("projectAi.effectiveModel", {
                  model: setting.effective.model,
                })
              : t("projectAi.defaultConfigured")
            : t("projectAi.unavailable")}
        </p>
      )}
      {observation && (
        <>
          <Checkbox
            label={t("projectAi.preferBrowser")}
            checked={preferBrowser}
            disabled={disabled}
            onChange={(_, data) => {
              setPreferBrowser(Boolean(data.checked));
              setNotice("");
            }}
          />
          <p>{t("projectAi.browserHelp")}</p>
        </>
      )}
      <div className="channel-form">
        <Field label={t("projectAi.mode")}>
          <Select
            value={mode}
            disabled={disabled}
            onChange={(_, data) => {
              setMode(data.value as typeof mode);
              setNotice("");
            }}
          >
            <option value="inherit">{t("projectAi.inherited")}</option>
            <option value="custom">{t("projectAi.configured")}</option>
          </Select>
        </Field>
        {mode === "custom" && (
          <>
            <Field label={t("projectAi.endpoint")} required>
              <Input
                value={baseUrl}
                disabled={disabled}
                autoComplete="off"
                onChange={(_, data) => {
                  setBaseUrl(data.value);
                  setNotice("");
                }}
              />
            </Field>
            <Field label={t("projectAi.model")} required>
              <Input
                value={model}
                disabled={disabled}
                autoComplete="off"
                onChange={(_, data) => {
                  setModel(data.value);
                  setNotice("");
                }}
              />
            </Field>
            <Button
              disabled={disabled || dirty || conflict}
              onClick={() => void discover()}
            >
              {t(
                busy === "discover"
                  ? "projectAi.discovering"
                  : "projectAi.discover",
              )}
            </Button>
            {models.length > 0 &&
              baseUrl === setting.base_url &&
              !apiKey &&
              mode === setting.mode && (
                <Field label={t("projectAi.discoveredModels")}>
                  <Select
                    disabled={disabled}
                    value={models.includes(model) ? model : ""}
                    onChange={(_, data) => {
                      if (data.value) {
                        setModel(data.value);
                        setNotice("");
                      }
                    }}
                  >
                    <option value="">{t("projectAi.selectModel")}</option>
                    {models.map((id) => (
                      <option key={id} value={id}>
                        {id}
                      </option>
                    ))}
                  </Select>
                </Field>
              )}
            {discoveryMessage && <p role="status">{discoveryMessage}</p>}
            <Field
              label={t("projectAi.apiKey")}
              hint={t(
                setting.key_present
                  ? "projectAi.keySaved"
                  : "projectAi.keyMissing",
              )}
            >
              <Input
                type="password"
                value={apiKey}
                disabled={disabled}
                autoComplete="new-password"
                onChange={(_, data) => {
                  setApiKey(data.value);
                  setNotice("");
                }}
              />
            </Field>
            <p>{t("projectAi.keyHelp")}</p>
          </>
        )}
      </div>
      {dirty && <p role="status">{t("projectAi.dirty")}</p>}
      {dirty && <p>{t("projectAi.saveFirst")}</p>}
      <div className="channel-row">
        <Button
          appearance="primary"
          disabled={disabled || !dirty || !valid || conflict}
          onClick={() => void save()}
        >
          {t(busy === "save" ? "projectAi.saving" : "projectAi.save")}
        </Button>
        <Button disabled={disabled || !dirty} onClick={reset}>
          {t("projectAi.reset")}
        </Button>
        <Button
          disabled={disabled || dirty || conflict}
          onClick={() => void test()}
        >
          {t(busy === "test" ? "projectAi.testing" : "projectAi.test")}
        </Button>
      </div>
      <p>{t("projectAi.testHelp")}</p>
      {error && <ErrorState title={error} />}
      {conflict && (
        <Button disabled={busy !== null} onClick={() => void onSaved()}>
          {t("projectAi.reload")}
        </Button>
      )}
      {notice && (
        <MessageBar intent="success">
          <MessageBarBody>{notice}</MessageBarBody>
        </MessageBar>
      )}
    </Card>
  );
}

export function ProjectAiSettingsPanel({
  tenantId,
  projectId,
}: {
  tenantId: string;
  projectId: string;
}) {
  const { t } = useTranslation();
  const { session } = useAuth();
  const query = useProjectAiSettings(tenantId, projectId);
  const canWrite =
    membershipForTenant(session, tenantId)?.role === "tenant_admin";
  return (
    <section className="channel-stack" aria-label={t("projectAi.title")}>
      <MessageBar intent="info">
        <MessageBarBody>{t("projectAi.boundary")}</MessageBarBody>
      </MessageBar>
      {!canWrite && <p>{t("projectAi.readOnly")}</p>}
      {query.isPending && <LoadingState label={t("projectAi.loading")} />}
      {query.isError && (
        <ErrorState
          title={t("projectAi.loadFailed")}
          onRetry={() => void query.refetch()}
        />
      )}
      {query.data?.items.map((setting) => (
        <AiSettingForm
          key={`${tenantId}:${projectId}:${setting.usage}`}
          setting={setting}
          tenantId={tenantId}
          projectId={projectId}
          canWrite={canWrite}
          onSaved={async () => !(await query.refetch()).isError}
        />
      ))}
    </section>
  );
}
