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
import { Navigate, useLocation, useNavigate } from "react-router-dom";
import { ApiError } from "../api/client";
import { ErrorState, LoadingState } from "../components/AsyncState";
import { useAuth } from "../auth/AuthProvider";

function returnToFrom(search: string) {
  const value = new URLSearchParams(search).get("returnTo");
  return value && value.startsWith("/") && !value.startsWith("//")
    ? value
    : "/workspaces";
}

export function LoginPage() {
  const { status, error, refresh, login } = useAuth();
  const location = useLocation();
  const navigate = useNavigate();
  const [loginName, setLoginName] = useState("");
  const [password, setPassword] = useState("");
  const [submitError, setSubmitError] = useState<string>();
  const [submitting, setSubmitting] = useState(false);
  const returnTo = returnToFrom(location.search);

  if (status === "checking") return <LoadingState label="正在确认登录状态" />;
  if (status === "authenticated") return <Navigate replace to={returnTo} />;
  if (status === "unavailable") {
    return (
      <main className="login-page">
        <ErrorState
          title="身份服务暂时不可用"
          detail={error?.message ?? "无法连接到身份服务。"}
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
          ? "用户名或密码不正确。"
          : apiError?.isRetryable
            ? "登录服务暂时不可用，请重试。"
            : (apiError?.message ?? "无法登录，请稍后重试。"),
      );
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <main className="login-page">
      <Card className="login-card">
        <div className="brand login-brand" aria-label="Memeloop GEO">
          <div className="brand-mark">M</div>
          <span>
            Memeloop <b>GEO</b>
          </span>
        </div>
        <div>
          <p className="eyebrow">欢迎回来</p>
          <h1>登录工作区</h1>
          <p>使用分配给你的 GEO 软件账号登录。</p>
        </div>
        {submitError && (
          <MessageBar intent="error" aria-live="assertive">
            <MessageBarBody>{submitError}</MessageBarBody>
          </MessageBar>
        )}
        <form className="login-form" onSubmit={submit}>
          <Field label="用户名" required>
            <Input
              autoComplete="username"
              value={loginName}
              onChange={(_, data) => setLoginName(data.value)}
              disabled={submitting}
              autoFocus
            />
          </Field>
          <Field label="密码" required>
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
            {submitting ? "正在登录…" : "登录"}
          </Button>
        </form>
        <p className="login-note">
          <LockClosedRegular aria-hidden="true" /> 会话由安全 Cookie
          保存；此浏览器不会保存密码或 CSRF 令牌。
        </p>
      </Card>
    </main>
  );
}
