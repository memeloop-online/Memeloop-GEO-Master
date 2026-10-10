import { useState } from "react";
import {
  Button,
  Card,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
} from "@fluentui/react-components";
import { ArrowRightRegular, LockClosedRegular } from "@fluentui/react-icons";
import { useTranslation } from "react-i18next";
import { Navigate, useLocation, useNavigate } from "react-router-dom";
import { ApiError } from "../api/client";
import { ErrorState, LoadingState } from "../components/AsyncState";
import { Brand } from "../components/Brand";
import { LanguageSelect } from "../components/LanguageSelect";
import { useAuth } from "../auth/AuthProvider";

function returnToFrom(search: string) {
  const value = new URLSearchParams(search).get("returnTo");
  return value && value.startsWith("/") && !value.startsWith("//")
    ? value
    : "/workspaces";
}

export function LoginPage() {
  const { t } = useTranslation();
  const { status, error, refresh, login } = useAuth();
  const location = useLocation();
  const navigate = useNavigate();
  const [loginName, setLoginName] = useState("");
  const [password, setPassword] = useState("");
  const [submitError, setSubmitError] = useState<string>();
  const [submitting, setSubmitting] = useState(false);
  const returnTo = returnToFrom(location.search);

  if (status === "checking")
    return <LoadingState label={t("login.checking")} />;
  if (status === "authenticated") return <Navigate replace to={returnTo} />;
  if (status === "unavailable") {
    return (
      <main className="login-page">
        <ErrorState
          title={t("login.unavailableTitle")}
          detail={error?.message ?? t("login.unavailableDetail")}
          onRetry={() => void refresh()}
        />
      </main>
    );
  }

  async function submit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setSubmitError(undefined);
    setSubmitting(true);
    try {
      await login({ login_name: loginName.trim(), password });
      navigate(returnTo, { replace: true });
    } catch (requestError) {
      const apiError =
        requestError instanceof ApiError ? requestError : undefined;
      setSubmitError(
        apiError?.status === 401
          ? t("login.invalidCredentials")
          : apiError?.isRetryable
            ? t("login.retry")
            : (apiError?.message ?? t("login.failed")),
      );
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <main className="login-page">
      <Card className="login-card">
        <Brand />
        <LanguageSelect />
        <div>
          <p className="eyebrow">{t("login.welcome")}</p>
          <h1>{t("login.title")}</h1>
          <p>{t("login.description")}</p>
        </div>
        {submitError && (
          <MessageBar intent="error" aria-live="assertive">
            <MessageBarBody>{submitError}</MessageBarBody>
          </MessageBar>
        )}
        <form className="login-form" onSubmit={submit}>
          <Field label={t("login.username")} required>
            <Input
              autoComplete="username"
              value={loginName}
              onChange={(_, data) => setLoginName(data.value)}
              disabled={submitting}
              autoFocus
            />
          </Field>
          <Field label={t("login.password")} required>
            <Input
              type="password"
              autoComplete="current-password"
              value={password}
              onChange={(_, data) => setPassword(data.value)}
              disabled={submitting}
            />
          </Field>
          <Button
            appearance="primary"
            type="submit"
            disabled={submitting || !loginName.trim() || !password}
            icon={submitting ? undefined : <ArrowRightRegular />}
          >
            {submitting ? t("login.submitting") : t("login.submit")}
          </Button>
        </form>
        <p className="login-note">
          <LockClosedRegular aria-hidden="true" /> {t("login.help")}
        </p>
      </Card>
    </main>
  );
}
