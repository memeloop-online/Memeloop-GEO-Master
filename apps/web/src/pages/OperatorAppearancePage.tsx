import { useCallback, useEffect, useState, type FormEvent } from "react";
import {
  Button,
  Card,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
  Select,
  Spinner,
} from "@fluentui/react-components";
import { Link } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { ApiError } from "../api/client";
import {
  getOperatorAppearance,
  updateOperatorAppearance,
  type Appearance,
  type UiLocale,
} from "../api/appearance";
import { useAppearance } from "../appearance/AppearanceProvider";
import { useAuth } from "../auth/AuthProvider";

export function OperatorAppearancePage() {
  const { t } = useTranslation();
  const { session } = useAuth();
  const { refreshAppearance } = useAppearance();
  const allowed = session?.memberships.some(
    (membership) => membership.role === "oem_admin",
  );
  const [appearance, setAppearance] = useState<Appearance>();
  const [name, setName] = useState("");
  const [color, setColor] = useState("");
  const [locale, setLocale] = useState<UiLocale>("zh-CN");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");

  const load = useCallback(async () => {
    setLoading(true);
    setError("");
    try {
      const next = await getOperatorAppearance();
      setAppearance(next);
      setName(next.display_name);
      setColor(next.primary_color);
      setLocale(next.default_locale);
    } catch (cause) {
      setError(
        cause instanceof Error ? cause.message : t("appearance.unavailable"),
      );
    } finally {
      setLoading(false);
    }
  }, [t]);

  useEffect(() => {
    if (allowed) void load();
    else setLoading(false);
  }, [allowed, load]);

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!appearance || saving) return;
    if (!/^#[0-9a-fA-F]{6}$/.test(color)) {
      setError(t("appearance.invalidColor"));
      return;
    }
    setSaving(true);
    setNotice("");
    setError("");
    try {
      const next = await updateOperatorAppearance(
        {
          display_name: name.trim(),
          primary_color: color,
          default_locale: locale,
        },
        appearance.revision,
      );
      setAppearance(next);
      setName(next.display_name);
      setColor(next.primary_color);
      setLocale(next.default_locale);
      await refreshAppearance();
      setNotice(t("appearance.saved"));
    } catch (cause) {
      setError(
        cause instanceof ApiError &&
          (cause.status === 409 || cause.status === 412)
          ? t("appearance.conflict")
          : cause instanceof Error
            ? cause.message
            : t("appearance.unavailable"),
      );
    } finally {
      setSaving(false);
    }
  }

  if (!allowed)
    return (
      <main>
        <h1>{t("appearance.unavailable")}</h1>
      </main>
    );
  return (
    <main style={{ maxWidth: 650, margin: "40px auto", padding: "0 16px" }}>
      <Link to="/ops/channels">{t("appearance.back")}</Link>
      <h1>{t("appearance.title")}</h1>
      <p>{t("appearance.description")}</p>
      {error && (
        <MessageBar intent="error" aria-live="assertive">
          <MessageBarBody>{error}</MessageBarBody>
        </MessageBar>
      )}
      {notice && (
        <MessageBar intent="success" aria-live="polite">
          <MessageBarBody>{notice}</MessageBarBody>
        </MessageBar>
      )}
      {loading ? (
        <Spinner label={t("appearance.loading")} />
      ) : appearance ? (
        <Card>
          <form
            onSubmit={(event) => void submit(event)}
            style={{ display: "grid", gap: 16 }}
          >
            <Field label={t("appearance.name")} required>
              <Input
                value={name}
                maxLength={200}
                onChange={(_, data) => setName(data.value)}
                required
              />
            </Field>
            <Field label={t("appearance.color")} required>
              <Input
                value={color}
                onChange={(_, data) => setColor(data.value)}
                required
                pattern="#[0-9a-fA-F]{6}"
              />
            </Field>
            <Field label={t("appearance.locale")}>
              <Select
                value={locale}
                onChange={(event) => setLocale(event.target.value as UiLocale)}
              >
                <option value="zh-CN">{t("appearance.zh")}</option>
                <option value="en">{t("appearance.en")}</option>
              </Select>
            </Field>
            <div style={{ display: "flex", gap: 8 }}>
              <Button
                appearance="primary"
                type="submit"
                disabled={saving || !name.trim()}
              >
                {saving ? t("appearance.saving") : t("appearance.save")}
              </Button>
              <Button
                type="button"
                onClick={() => void load()}
                disabled={saving}
              >
                {t("appearance.reload")}
              </Button>
            </div>
          </form>
        </Card>
      ) : (
        <Button onClick={() => void load()}>{t("appearance.reload")}</Button>
      )}
    </main>
  );
}
