import { useQueryClient } from "@tanstack/react-query";
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import {
  getSession,
  login as requestLogin,
  logout as requestLogout,
  type LoginInput,
} from "../api/auth";
import {
  advanceAuthGeneration,
  ApiError,
  getAuthGeneration,
  setCsrfToken,
  setUnauthorizedHandler,
} from "../api/client";
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
  const verification = useRef<
    { generation: number; promise: Promise<void> } | undefined
  >(undefined);

  const clearLocalSession = useCallback(() => {
    advanceAuthGeneration();
    setCsrfToken(undefined);
    setSession(undefined);
    setError(undefined);
    setStatus("anonymous");
    // clear() destroys/cancels the old queries synchronously. Do not wait for
    // cancellation and then accidentally clear a subsequent login's cache.
    queryClient.clear();
  }, [queryClient]);

  const acceptSession = useCallback((nextSession: AuthSession) => {
    advanceAuthGeneration();
    setCsrfToken(nextSession.csrf_token);
    setSession(nextSession);
    setError(undefined);
    setStatus("authenticated");
    return nextSession;
  }, []);

  const refresh = useCallback(async () => {
    const generation = advanceAuthGeneration();
    setStatus("checking");
    setError(undefined);
    try {
      const nextSession = await getSession();
      if (generation !== getAuthGeneration()) return;
      acceptSession(nextSession);
    } catch (requestError) {
      if (generation !== getAuthGeneration()) return;
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
    async (input: LoginInput) => {
      const generation = advanceAuthGeneration();
      try {
        const nextSession = await requestLogin(input);
        if (generation === getAuthGeneration()) acceptSession(nextSession);
        return nextSession;
      } catch (requestError) {
        // This attempt superseded any pending refresh. Settle authentication
        // from a fresh session read while returning the original login error.
        if (generation === getAuthGeneration()) void refresh();
        throw requestError;
      }
    },
    [acceptSession, refresh],
  );

  const logout = useCallback(async () => {
    const generation = advanceAuthGeneration();
    try {
      await requestLogout();
    } catch (requestError) {
      const apiError = errorFrom(requestError);
      if (apiError.status !== 401) throw apiError;
    }
    if (generation === getAuthGeneration()) clearLocalSession();
  }, [clearLocalSession]);

  const verifyUnauthorized = useCallback(
    (generation: number) => {
      if (generation !== getAuthGeneration()) return;
      if (verification.current?.generation === generation) {
        return verification.current.promise;
      }
      const promise = (async () => {
        try {
          // A business endpoint may return 401 for its own resource/upstream.
          // Only the first-party session endpoint can confirm session loss.
          await getSession();
        } catch (requestError) {
          if (
            generation === getAuthGeneration() &&
            errorFrom(requestError).status === 401
          ) {
            clearLocalSession();
          }
        } finally {
          if (verification.current?.generation === generation) {
            verification.current = undefined;
          }
        }
      })();
      verification.current = { generation, promise };
      return promise;
    },
    [clearLocalSession],
  );

  useEffect(() => {
    setUnauthorizedHandler(verifyUnauthorized);
    void refresh();
    return () => {
      advanceAuthGeneration();
      setUnauthorizedHandler(undefined);
    };
  }, [refresh, verifyUnauthorized]);

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
