import { consumerWebProviders } from "./consumer-web-providers.mjs";

// Public consumer client request contracts inspected 2026-10-10. These are
// read-only account probes, not inference, token-refresh or login protocols.
const probes = Object.freeze({
  qianwen: {
    // Official web SDK's read-only membership/profile request. This is the
    // sole cross-origin exception; neither origin nor endpoint is overridable.
    url: "https://api.qianwen.com/growth/user/benefit/user/member/info",
    method: "POST",
    body: '{"clientChannel":"PC"}',
    headers: { "Content-Type": "application/json" },
    credential: "session",
  },
  glm: {
    path: "/chatglm/user-api/user/info",
    headers: {
      "Content-Type": "application/x-www-form-urlencoded",
      "App-Name": "chatglm",
    },
    credential: "cookie",
    key: "chatglm_token",
  },
  deepseek: {
    path: "/api/v0/users/current",
    headers: {},
    credential: "storage",
    key: "userToken",
  },
  doubao: {
    path: "/passport/account/info/v2/?aid=497858&account_sdk_source=web&sdk_version=2.2.11-doubao.0&device_platform=web",
    headers: { "Agw-Js-Conv": "str" },
    credential: "session",
    // The website SDK may append these signatures to its own fetch. Never
    // copy their values, construct signatures, or accept changed base params.
    signedQueryParameters: ["msToken", "a_bogus"],
  },
});

function identity(id, name, fallback = false) {
  if (typeof id !== "string" || !id.trim()) return null;
  const displayName = typeof name === "string" ? name.trim() : "";
  if (!displayName && !fallback) return null;
  return {
    platform_account_id: id,
    // No email/mobile fallback: the label is not evidence of identity.
    display_name: displayName || `Account · ${id.slice(-6)}`,
  };
}

export function glmIdentity(data) {
  if (data?.status !== 0 || data.result?.is_guest !== false) return null;
  return identity(data.result._id, data.result.username);
}

export function deepseekIdentity(data) {
  if (data?.code !== 0 || data.data?.biz_code !== 0) return null;
  const user = data.data.biz_data;
  if (!user || (user.is_guest !== undefined && user.is_guest !== false))
    return null;
  const id =
    typeof user.id === "number" && Number.isSafeInteger(user.id) && user.id > 0
      ? String(user.id)
      : user.id;
  return identity(id, user.id_profile?.name, true);
}

export function doubaoIdentity(data) {
  const user = data?.data;
  if (
    data?.message !== "success" ||
    !user ||
    (user.error_code !== undefined && user.error_code !== 0) ||
    typeof user.sec_user_id !== "string" ||
    !user.sec_user_id.trim() ||
    (user.is_guest !== undefined && user.is_guest !== false)
  )
    return null;
  return identity(user.user_id_str, user.name, true);
}

export function qianwenIdentity(data) {
  if (data?.success !== true || data?.failed !== false) return null;
  return identity(data.data?.userId, data.data?.userNick, true);
}

const identities = {
  glm: glmIdentity,
  deepseek: deepseekIdentity,
  doubao: doubaoIdentity,
  qianwen: qianwenIdentity,
};

async function probeAccount(page, provider, trustedOrigin) {
  try {
    const data = await page.evaluate(
      async ({ origin, provider, spec }) => {
        if (location.origin !== origin) return null;
        const headers = { ...spec.headers };
        if (spec.credential !== "session") {
          let token;
          if (spec.credential === "cookie") {
            const cookie = document.cookie
              .split(";")
              .map((part) => part.trim())
              .find((part) => part.startsWith(`${spec.key}=`));
            if (!cookie) return null;
            token = decodeURIComponent(cookie.slice(spec.key.length + 1));
          } else {
            const stored = JSON.parse(localStorage.getItem(spec.key) ?? "null");
            if (stored?.__version !== "0") return null;
            token = stored.value;
          }
          if (typeof token !== "string" || !token.trim()) return null;
          headers.Authorization = `Bearer ${token}`;
        }
        const response = await fetch(spec.url ?? spec.path, {
          method: spec.method ?? "GET",
          credentials: provider === "qianwen" ? "include" : "same-origin",
          ...(spec.body ? { body: spec.body } : {}),
          redirect: "error",
          signal: AbortSignal.timeout(10_000),
          headers,
        });
        const matchesResponseUrl = () => {
          const expected = spec.url ?? `${origin}${spec.path}`;
          if (!spec.signedQueryParameters) return response.url === expected;
          try {
            const actual = new URL(response.url);
            const target = new URL(expected);
            if (
              actual.origin !== target.origin ||
              actual.pathname !== target.pathname ||
              actual.username ||
              actual.password ||
              response.url.includes("#")
            )
              return false;
            for (const [key, value] of target.searchParams) {
              const values = actual.searchParams.getAll(key);
              if (values.length !== 1 || values[0] !== value) return false;
            }
            for (const key of actual.searchParams.keys()) {
              if (actual.searchParams.getAll(key).length !== 1) return false;
              if (
                !target.searchParams.has(key) &&
                !spec.signedQueryParameters.includes(key)
              )
                return false;
            }
            return true;
          } catch {
            return false;
          }
        };
        if (
          response.status !== 200 ||
          response.redirected ||
          !matchesResponseUrl() ||
          !/^application\/json(?:\s*;|$)/i.test(
            response.headers.get("content-type") ?? "",
          )
        )
          return null;
        const reader = response.body?.getReader();
        if (!reader) return null;
        const decoder = new TextDecoder();
        let text = "";
        let bytes = 0;
        for (;;) {
          const { done, value } = await reader.read();
          if (done) break;
          bytes += value.byteLength;
          if (bytes > 128_000) {
            await reader.cancel();
            return null;
          }
          text += decoder.decode(value, { stream: true });
        }
        text += decoder.decode();
        const body = JSON.parse(text);
        const scalar = (value) =>
          value === undefined
            ? undefined
            : ["string", "number", "boolean"].includes(typeof value)
              ? value
              : null;
        // Only explicit identity fields leave the page. Do not transfer tokens,
        // email, mobile, response diagnostics, or unrelated profile fields.
        if (provider === "qianwen")
          return {
            success: scalar(body?.success),
            failed: scalar(body?.failed),
            data: {
              userId: scalar(body?.data?.userId),
              userNick: scalar(body?.data?.userNick),
            },
          };
        if (provider === "glm")
          return {
            status: scalar(body?.status),
            result: {
              _id: scalar(body?.result?._id),
              username: scalar(body?.result?.username),
              is_guest: scalar(body?.result?.is_guest),
            },
          };
        if (provider === "deepseek")
          return {
            code: scalar(body?.code),
            data: {
              biz_code: scalar(body?.data?.biz_code),
              biz_data: {
                id: scalar(body?.data?.biz_data?.id),
                is_guest: scalar(body?.data?.biz_data?.is_guest),
                id_profile: {
                  name: scalar(body?.data?.biz_data?.id_profile?.name),
                },
              },
            },
          };
        return {
          message: scalar(body?.message),
          data: {
            error_code: scalar(body?.data?.error_code),
            user_id_str: scalar(body?.data?.user_id_str),
            sec_user_id: scalar(body?.data?.sec_user_id),
            name: scalar(body?.data?.name),
            is_guest: scalar(body?.data?.is_guest),
          },
        };
      },
      { origin: trustedOrigin, provider, spec: probes[provider] },
    );
    return identities[provider](data);
  } catch {
    return null;
  }
}

export const probeGlmAccount = (
  page,
  { trustedOrigin = consumerWebProviders.glm.origin } = {},
) => probeAccount(page, "glm", trustedOrigin);
export const probeDeepseekAccount = (
  page,
  { trustedOrigin = consumerWebProviders.deepseek.origin } = {},
) => probeAccount(page, "deepseek", trustedOrigin);
export const probeDoubaoAccount = (
  page,
  { trustedOrigin = consumerWebProviders.doubao.origin } = {},
) => probeAccount(page, "doubao", trustedOrigin);
// Deliberately no origin/endpoint override: test via browser routing instead.
export const probeQianwenAccount = (page) =>
  probeAccount(page, "qianwen", consumerWebProviders.qianwen.origin);
