import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { apiFetch, apiFetchBlob, setCsrfToken } from "../api/client";
import { AuthProvider, useAuth } from "./AuthProvider";
import type { AuthSession } from "./types";

const session: AuthSession = {
  user: { id: "user-a", login_name: "test", display_name: "Test" },
  operator: { id: "operator-a", slug: "test", display_name: "Test" },
  memberships: [],
  expires_at: null,
  csrf_token: "csrf-a",
};
const newSession = { ...session, csrf_token: "csrf-new" };
const input = { login_name: "test", password: "test-only" };

function response(status: number, body: unknown = {}) {
  return new Response(JSON.stringify(body), { status });
}

function deferred() {
  let resolve!: (value: Response) => void;
  const promise = new Promise<Response>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

function mount() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const hook = renderHook(() => useAuth(), {
    wrapper: ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={client}>
        <AuthProvider>{children}</AuthProvider>
      </QueryClientProvider>
    ),
  });
  return { ...hook, client };
}

afterEach(() => {
  cleanup();
  setCsrfToken(undefined);
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("first-party authentication generation", () => {
  it.each([200, 401, 503])(
    "settles a failed login that superseded initial refresh with session HTTP %s",
    async (status) => {
      const initial = deferred();
      const settlement = deferred();
      const fetchMock = vi
        .fn()
        .mockReturnValueOnce(initial.promise)
        .mockResolvedValueOnce(response(401, { message: "Login rejected" }))
        .mockReturnValueOnce(settlement.promise);
      vi.stubGlobal("fetch", fetchMock);
      const { result } = mount();
      await act(async () => {
        await expect(result.current.login(input)).rejects.toMatchObject({
          status: 401,
          message: "Login rejected",
        });
      });
      expect(fetchMock.mock.calls[2][0]).toContain("/auth/session");
      await act(async () => {
        initial.resolve(response(200, session));
        settlement.resolve(response(status, newSession));
      });
      expect(result.current.status).toBe(
        status === 200
          ? "authenticated"
          : status === 401
            ? "anonymous"
            : "unavailable",
      );
      expect(result.current.session).toEqual(
        status === 200 ? newSession : undefined,
      );
    },
  );

  it("settles a failed login that superseded an explicit refresh", async () => {
    const oldRefresh = deferred();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(response(200, session))
      .mockReturnValueOnce(oldRefresh.promise)
      .mockResolvedValueOnce(response(401, { message: "Login rejected" }))
      .mockResolvedValueOnce(response(200, session));
    vi.stubGlobal("fetch", fetchMock);
    const { result } = mount();
    await waitFor(() => expect(result.current.status).toBe("authenticated"));
    let pendingRefresh!: Promise<void>;
    act(() => {
      pendingRefresh = result.current.refresh();
    });
    await act(async () => {
      await expect(result.current.login(input)).rejects.toMatchObject({
        status: 401,
      });
    });
    await waitFor(() => expect(result.current.status).toBe("authenticated"));
    await act(async () => {
      oldRefresh.resolve(response(401));
      await pendingRefresh;
    });
    expect(result.current.session).toEqual(session);
  });

  it("does not let failed-login settlement overwrite a newer login", async () => {
    const initial = deferred();
    const settlement = deferred();
    const fetchMock = vi
      .fn()
      .mockReturnValueOnce(initial.promise)
      .mockResolvedValueOnce(response(401))
      .mockReturnValueOnce(settlement.promise)
      .mockResolvedValueOnce(response(200, newSession));
    vi.stubGlobal("fetch", fetchMock);
    const { result } = mount();
    await act(async () => {
      await expect(result.current.login(input)).rejects.toMatchObject({
        status: 401,
      });
      await result.current.login(input);
    });
    await act(async () => {
      initial.resolve(response(401));
      settlement.resolve(response(401));
    });
    expect(result.current.status).toBe("authenticated");
    expect(result.current.session).toEqual(newSession);
  });

  it("does not start failed-login settlement after a newer refresh", async () => {
    const initial = deferred();
    const login = deferred();
    const fetchMock = vi
      .fn()
      .mockReturnValueOnce(initial.promise)
      .mockReturnValueOnce(login.promise)
      .mockResolvedValueOnce(response(200, newSession));
    vi.stubGlobal("fetch", fetchMock);
    const { result } = mount();
    const pendingLogin = result.current
      .login(input)
      .catch((error: unknown) => error);
    await act(async () => {
      await result.current.refresh();
    });
    await act(async () => {
      login.resolve(response(401));
      await pendingLogin;
      initial.resolve(response(401));
    });
    expect(fetchMock).toHaveBeenCalledTimes(3);
    expect(result.current.status).toBe("authenticated");
    expect(result.current.session).toEqual(newSession);
  });

  it.each(["json", "binary"] as const)(
    "ignores a late %s 401 after a newer login",
    async (kind) => {
      const oldRequest = deferred();
      const fetchMock = vi
        .fn()
        .mockResolvedValueOnce(response(200, session))
        .mockReturnValueOnce(oldRequest.promise)
        .mockResolvedValueOnce(response(200, newSession))
        .mockResolvedValueOnce(response(200));
      vi.stubGlobal("fetch", fetchMock);
      const { result, client } = mount();
      await waitFor(() => expect(result.current.status).toBe("authenticated"));
      const pending = (
        kind === "json" ? apiFetch("/resource") : apiFetchBlob("/image")
      ).catch((error: unknown) => error);
      await act(async () => {
        await result.current.login(input);
      });
      client.setQueryData(["new-session"], "keep");
      await act(async () => {
        oldRequest.resolve(response(401));
        await pending;
      });
      expect(result.current.session).toEqual(newSession);
      expect(client.getQueryData(["new-session"])).toBe("keep");
      expect(fetchMock).toHaveBeenCalledTimes(3);
      await apiFetch("/write", { method: "POST" });
      expect(fetchMock.mock.calls[3][1].headers.get("X-CSRF-Token")).toBe(
        "csrf-new",
      );
    },
  );

  it.each([200, 401, 503])(
    "deduplicates business 401 verification and handles session HTTP %s",
    async (status) => {
      const check = deferred();
      const fetchMock = vi.fn().mockImplementation((url: string) => {
        if (url.endsWith("/auth/session")) {
          return fetchMock.mock.calls.filter(([path]) => path === url)
            .length === 1
            ? Promise.resolve(response(200, session))
            : check.promise;
        }
        return Promise.resolve(response(401));
      });
      vi.stubGlobal("fetch", fetchMock);
      const { result, client } = mount();
      await waitFor(() => expect(result.current.status).toBe("authenticated"));
      client.setQueryData(["protected"], "cached");
      await act(async () => {
        await Promise.all([
          apiFetch("/resource").catch(() => undefined),
          apiFetchBlob("/image").catch(() => undefined),
        ]);
      });
      expect(
        fetchMock.mock.calls.filter(([url]) => url.endsWith("/auth/session")),
      ).toHaveLength(2);
      await act(async () => {
        check.resolve(response(status, status === 200 ? session : {}));
      });
      await waitFor(() =>
        expect(result.current.status).toBe(
          status === 401 ? "anonymous" : "authenticated",
        ),
      );
      expect(client.getQueryData(["protected"])).toBe(
        status === 401 ? undefined : "cached",
      );
      expect(fetchMock).toHaveBeenCalledTimes(4);
    },
  );

  it.each([200, 401, 503])(
    "ignores stale refresh HTTP %s after login",
    async (status) => {
      const refresh = deferred();
      const fetchMock = vi
        .fn()
        .mockReturnValueOnce(refresh.promise)
        .mockResolvedValueOnce(response(200, newSession));
      vi.stubGlobal("fetch", fetchMock);
      const { result, client } = mount();
      await act(async () => {
        await result.current.login(input);
      });
      client.setQueryData(["new-session"], "keep");
      await act(async () => {
        refresh.resolve(response(status, session));
      });
      expect(result.current.status).toBe("authenticated");
      expect(result.current.session).toEqual(newSession);
      expect(client.getQueryData(["new-session"])).toBe("keep");
    },
  );

  it("ignores a pending unauthorized verification after a new login", async () => {
    const check = deferred();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(response(200, session))
      .mockResolvedValueOnce(response(401))
      .mockReturnValueOnce(check.promise)
      .mockResolvedValueOnce(response(200, newSession));
    vi.stubGlobal("fetch", fetchMock);
    const { result, client } = mount();
    await waitFor(() => expect(result.current.status).toBe("authenticated"));
    await act(async () => {
      await apiFetch("/resource").catch(() => undefined);
    });
    await act(async () => {
      await result.current.login(input);
    });
    client.setQueryData(["new-session"], "keep");
    await act(async () => {
      check.resolve(response(401));
    });
    expect(result.current.session).toEqual(newSession);
    expect(client.getQueryData(["new-session"])).toBe("keep");
  });

  it("does not defer old cache clearing until after a new login", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(response(200, session))
      .mockResolvedValueOnce(response(401))
      .mockResolvedValueOnce(response(401))
      .mockResolvedValueOnce(response(200, newSession));
    vi.stubGlobal("fetch", fetchMock);
    const { result, client } = mount();
    await waitFor(() => expect(result.current.status).toBe("authenticated"));
    let release!: () => void;
    const cancel = vi.spyOn(client, "cancelQueries").mockImplementation(
      () =>
        new Promise<void>((resolve) => {
          release = resolve;
        }),
    );
    client.setQueryData(["old-session"], "old");
    await act(async () => {
      await apiFetch("/resource").catch(() => undefined);
    });
    await waitFor(() => expect(result.current.status).toBe("anonymous"));
    expect(client.getQueryData(["old-session"])).toBeUndefined();
    await act(async () => {
      await result.current.login(input);
    });
    client.setQueryData(["new-session"], "keep");
    await act(async () => {
      release?.();
    });
    expect(client.getQueryData(["new-session"])).toBe("keep");
    expect(result.current.session).toEqual(newSession);
    expect(cancel).not.toHaveBeenCalled();
  });
});
