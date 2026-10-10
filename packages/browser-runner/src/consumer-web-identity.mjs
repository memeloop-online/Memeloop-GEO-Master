import { consumerWebProviders } from "./consumer-web-providers.mjs";

// Public consumer client request contracts inspected 2026-10-10. These are
// read-only account probes, not inference, token-refresh or login protocols.
const probes = Object.freeze({
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

const identities = {
  glm: glmIdentity,
  deepseek: deepseekIdentity,
  doubao: doubaoIdentity,
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
        const response = await fetch(spec.path, {
          method: "GET",
          credentials: "same-origin",
          redirect: "error",
          signal: AbortSignal.timeout(10_000),
          headers,
        });
        if (
          response.status !== 200 ||
          response.url !== `${origin}${spec.path}` ||
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
