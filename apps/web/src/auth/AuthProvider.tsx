import { useQueryClient } from "@tanstack/react-query";
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import {
  getSession,
  login as requestLogin,
  logout as requestLogout,
  type LoginInput,
} from "../api/auth";
import { ApiError, setCsrfToken, setUnauthorizedHandler } from "../api/client";
import type { AuthSession } from "./types";

export type AuthStatus =
  "checking" | "anonymous" | "authenticated" | "unavailable";

interface AuthContextValue {
  status: AuthStatus;
  session?: AuthSession;
  error?: ApiError;
  refresh: () => Promise<void>;
  login: (input: LoginInput) => Promise<AuthSession>;
  logout: () => Promise<void>;
}

const AuthContext = createContext<AuthContextValue | undefined>(undefined);

function errorFrom(error: unknown): ApiError {
  if (error instanceof ApiError) return error;
  return new ApiError(
    0,
    {},
    error instanceof Error ? error.message : "身份服务暂不可用",
  );
}

export function AuthProvider({ children }: { children: ReactNode }) {
  const queryClient = useQueryClient();
  const [status, setStatus] = useState<AuthStatus>("checking");
  const [session, setSession] = useState<AuthSession>();
  const [error, setError] = useState<ApiError>();

  const clearLocalSession = useCallback(async () => {
    setCsrfToken(undefined);
    setSession(undefined);
    setError(undefined);
    setStatus("anonymous");
    await queryClient.cancelQueries();
    queryClient.clear();
  }, [queryClient]);

  const acceptSession = useCallback((nextSession: AuthSession) => {
    setCsrfToken(nextSession.csrf_token);
    setSession(nextSession);
    setError(undefined);
    setStatus("authenticated");
    return nextSession;
  }, []);

  const refresh = useCallback(async () => {
    setStatus("checking");
    setError(undefined);
    try {
      acceptSession(await getSession());
    } catch (requestError) {
      const apiError = errorFrom(requestError);
      if (apiError.status === 401) {
        await clearLocalSession();
        return;
      }
      setCsrfToken(undefined);
      setSession(undefined);
      setError(apiError);
      setStatus("unavailable");
    }
  }, [acceptSession, clearLocalSession]);

  const login = useCallback(
    async (input: LoginInput) => acceptSession(await requestLogin(input)),
    [acceptSession],
  );

  const logout = useCallback(async () => {
    try {
      await requestLogout();
    } catch (requestError) {
      const apiError = errorFrom(requestError);
      if (apiError.status !== 401) throw apiError;
    }
    await clearLocalSession();
  }, [clearLocalSession]);

  useEffect(() => {
    setUnauthorizedHandler(clearLocalSession);
    void refresh();
    return () => setUnauthorizedHandler(undefined);
  }, [clearLocalSession, refresh]);

  const value = useMemo<AuthContextValue>(
    () => ({ status, session, error, refresh, login, logout }),
    [error, login, logout, refresh, session, status],
  );

  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>;
}

export function useAuth() {
  const value = useContext(AuthContext);
  if (!value) throw new Error("useAuth 必须在 AuthProvider 内使用");
  return value;
}
