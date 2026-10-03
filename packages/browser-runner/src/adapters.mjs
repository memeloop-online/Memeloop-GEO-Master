// Fixed platform URLs and source-derived selectors. This connector version is
// not live-verified: no authenticated account was used to prove these flows.
import { createHash } from "node:crypto";

export const CONNECTOR_VERSION = "live_unverified.source_derived.v1";

const ZHIHU_SELF = "https://www.zhihu.com/api/v4/me";
const BAIDU_SELF = "https://baijiahao.baidu.com/builder/app/appinfo";
const ZHIHU_EDITOR = "https://zhuanlan.zhihu.com/write";
const BAIDU_EDITOR = "https://baijiahao.baidu.com/builder/rc/edit?type=news";
const ZHIHU_POST_ORIGIN = "https://zhuanlan.zhihu.com";
const KIMI_ORIGIN = "https://www.kimi.com";
// Public Kimi client bundle (2026-09-29) invokes
// kimi.gateway.account.v1.UserService/GetCurrentUser via Connect JSON.
// The authenticated response has not been observed with a real account.
const KIMI_SELF = "/apiv2/kimi.gateway.account.v1.UserService/GetCurrentUser";

function unsupported(reason) {
  return {
    status: "unsupported",
    reason,
    evidence: [],
    connector_version: CONNECTOR_VERSION,
  };
}

function unknown(reason, stage, evidence = []) {
  return {
    status: "unknown",
    reason,
    stage,
    evidence,
    connector_version: CONNECTOR_VERSION,
  };
}

function ownIdentity(id, name, avatar) {
  if (
    (typeof id !== "string" && typeof id !== "number") ||
    !String(id).trim() ||
    typeof name !== "string" ||
    !name.trim()
  ) {
    return null;
  }
  return {
    platform_account_id: String(id),
    display_name: name.trim(),
    ...(typeof avatar === "string" && avatar.startsWith("https://")
      ? { avatar_url: avatar }
      : {}),
  };
}

// A temporary *browser page*, not Node fetch or APIRequestContext, guarantees
// the same BrowserContext cookies and configured browser proxy for the probe.
export async function probeOwnAccount(page, endpoint, extract) {
  const probe = await page.context().newPage();
  try {
    const response = await probe.goto(endpoint, {
      waitUntil: "domcontentloaded",
      timeout: 12_000,
    });
    if (!response || response.status() !== 200) return null;
    const actual = new URL(probe.url());
    const expected = new URL(endpoint);
    if (
      actual.origin !== expected.origin ||
      actual.pathname !== expected.pathname
    ) {
      return null;
    }
    if (!/application\/json/i.test(response.headers()["content-type"] ?? ""))
      return null;
    const text = await response.text();
    if (text.length > 128_000) return null;
    return extract(JSON.parse(text));
  } catch {
    return null;
  } finally {
    await probe.close();
  }
}

function digest(title, body) {
  return createHash("sha256")
    .update(`${normalized(title)}\n${normalized(body)}`, "utf8")
    .digest("hex");
}

export function zhihuIdentity(data) {
  return ownIdentity(data?.id, data?.name, data?.avatar_url);
}

export function baiduIdentity(data) {
  const user = data?.data?.user;
  return ownIdentity(user?.userid, user?.name, user?.avatar);
}

export function kimiIdentity(data) {
  return ownIdentity(data?.user?.id, data?.user?.nickname);
}

export async function probeKimiAccount(
  page,
  { trustedOrigin = KIMI_ORIGIN } = {},
) {
  const probe = await page.context().newPage();
  try {
    const response = await probe.goto(`${trustedOrigin}/`, {
      waitUntil: "domcontentloaded",
      timeout: 12_000,
    });
    if (
      response?.status() !== 200 ||
      new URL(probe.url()).origin !== trustedOrigin
    ) {
      return null;
    }
    // Fixed, same-origin Connect request. Token never leaves the browser page
    // and is never returned to Node, logs, the HTTP caller or another origin.
    // This mirrors the public client's getToken()/getCurrentUser() path.
    const data = await probe.evaluate(async (selfPath) => {
      const accessToken = localStorage.getItem("access_token");
      const refreshToken = localStorage.getItem("refresh_token");
      if (!accessToken || !refreshToken) return null;
      const response = await fetch(selfPath, {
        method: "POST",
        credentials: "same-origin",
        headers: {
          Authorization: `Bearer ${accessToken}`,
          "Content-Type": "application/json",
          "Connect-Protocol-Version": "1",
          "x-msh-platform": "web",
        },
        body: "{}",
        redirect: "error",
      });
      if (
        !response.ok ||
        !/application\/json/i.test(response.headers.get("content-type") ?? "")
      ) {
        return null;
      }
      const text = await response.text();
      return text.length <= 128_000 ? JSON.parse(text) : null;
    }, KIMI_SELF);
    return kimiIdentity(data);
  } catch {
    return null;
  } finally {
    await probe.close();
  }
}

// This parses a candidate *observation*, not proof that Kimi ran its official
// search. An authenticated account, search-mode selection and an actual search
// event must be established independently before a caller can mark it complete.
// In particular, a plausible answer with links is not search evidence.
export function extractKimiCandidateObservation(value) {
  if (
    value === null ||
    typeof value !== "object" ||
    Array.isArray(value) ||
    typeof value.raw_answer !== "string" ||
    !value.raw_answer.trim() ||
    value.raw_answer.length > 100_000 ||
    !Array.isArray(value.citations) ||
    value.citations.length > 50 ||
    (value.request_id !== undefined &&
      (typeof value.request_id !== "string" ||
        !/^[\w-]{1,128}$/u.test(value.request_id)))
  ) {
    return null;
  }
  const citations = [];
  for (const citation of value.citations) {
    if (
      citation === null ||
      typeof citation !== "object" ||
      Array.isArray(citation) ||
      typeof citation.url !== "string" ||
      citation.url.length > 2048 ||
      (citation.title !== undefined &&
        (typeof citation.title !== "string" || citation.title.length > 500))
    ) {
      return null;
    }
    let url;
    try {
      url = new URL(citation.url);
    } catch {
      return null;
    }
    if (
      url.protocol !== "https:" ||
      url.username ||
      url.password ||
      !url.hostname ||
      url.hostname === "localhost" ||
      url.hostname.endsWith(".localhost")
    ) {
      return null;
    }
    citations.push({
      url: url.href,
      ...(citation.title ? { title: citation.title } : {}),
    });
  }
  return {
    raw_answer: value.raw_answer,
    citations,
    ...(value.request_id ? { request_id: value.request_id } : {}),
    search_verified: false,
  };
}

export async function xiaohongshuIdentity(
  page,
  { trustedOrigin = "https://creator.xiaohongshu.com" } = {},
) {
  try {
    const current = new URL(page.url());
    if (
      current.origin !== trustedOrigin ||
      !(
        current.pathname === "/" ||
        current.pathname.startsWith("/new/") ||
        current.pathname.startsWith("/publish/")
      )
    ) {
      return null;
    }
    // Scoped to the current creator dashboard's own-user control, never a
    // public note/author link supplied by a caller.
    const own = page.locator(".main-container .user");
    const href = await own
      .locator("a[href^='/user/profile/']")
      .first()
      .getAttribute("href", {
        timeout: 2_000,
      });
    const id = href?.match(/^\/user\/profile\/([\w-]{1,128})\/?$/u)?.[1];
    const name = await own
      .locator(".user-name")
      .first()
      .textContent({ timeout: 2_000 });
    return ownIdentity(id, name);
  } catch {
    return null;
  }
}

function contentPayload(payload, allowUrl = false) {
  if (
    payload === null ||
    typeof payload !== "object" ||
    Array.isArray(payload) ||
    Object.keys(payload).some(
      (key) =>
        key !== "title" &&
        key !== "body" &&
        !(allowUrl && key === "public_url"),
    ) ||
    typeof payload.title !== "string" ||
    typeof payload.body !== "string"
  ) {
    return null;
  }
  const title = payload.title.trim();
  const body = payload.body.trim();
  if (!title || !body || title.length > 150 || body.length > 100_000)
    return null;
  return { title, body };
}

function normalized(text) {
  return text.replace(/\s+/gu, " ").trim();
}

function zhihuPostUrl(value, origin = ZHIHU_POST_ORIGIN) {
  try {
    const url = new URL(value);
    return url.origin === origin && /^\/p\/\d+\/?$/.test(url.pathname)
      ? `${url.origin}${url.pathname}`
      : null;
  } catch {
    return null;
  }
}

async function ownZhihuArticle(
  page,
  postId,
  title,
  {
    selfUrl = ZHIHU_SELF,
    articlesOrigin = "https://www.zhihu.com",
    expectedAccountId,
  } = {},
) {
  const self = await probeOwnAccount(page, selfUrl, (data) => data);
  if (
    expectedAccountId !== undefined &&
    zhihuIdentity(self)?.platform_account_id !== expectedAccountId
  ) {
    return false;
  }
  const token = self?.url_token;
  if (typeof token !== "string" || !/^[\w-]{1,128}$/u.test(token)) return false;
  const endpoint = `${articlesOrigin}/api/v4/members/${encodeURIComponent(token)}/articles?limit=20&offset=0`;
  const articles = await probeOwnAccount(page, endpoint, (data) => data?.data);
  return (
    Array.isArray(articles) &&
    articles.some(
      (article) =>
        String(article?.id) === postId &&
        normalized(article?.title ?? "") === title,
    )
  );
}

export async function readbackZhihu(
  page,
  url,
  expected,
  {
    postOrigin = ZHIHU_POST_ORIGIN,
    selfUrl = ZHIHU_SELF,
    articlesOrigin = "https://www.zhihu.com",
    proxy,
    expectedAccountId,
  } = {},
) {
  const publicUrl = zhihuPostUrl(url, postOrigin);
  if (!publicUrl) return unknown("public_url_unverified", "readback");
  let publicContext;
  try {
    // Public verification must not inherit account cookies/local storage.
    // Use a new browser context with the same assigned proxy, never direct fallback.
    publicContext = await page
      .context()
      .browser()
      .newContext(proxy ? { proxy } : {});
    const readback = await publicContext.newPage();
    const response = await readback.goto(publicUrl, {
      waitUntil: "domcontentloaded",
      timeout: 15_000,
    });
    if (
      !response ||
      response.status() !== 200 ||
      zhihuPostUrl(readback.url(), postOrigin) !== publicUrl
    ) {
      return unknown("public_readback_unavailable", "readback");
    }
    const heading = await readback
      .locator("h1")
      .first()
      .textContent({ timeout: 3_000 });
    const article = readback
      .locator("article, .Post-RichText, .RichText")
      .first();
    const body = await article.innerText({ timeout: 3_000 });
    const observedTitle = normalized(heading ?? "");
    const observedBody = normalized(body);
    const expectedTitle = normalized(expected.title);
    const expectedBody = normalized(expected.body);
    if (observedTitle !== expectedTitle || observedBody !== expectedBody) {
      return unknown("public_content_mismatch", "readback");
    }
    const postId = new URL(publicUrl).pathname.match(/^\/p\/(\d+)/u)?.[1];
    if (
      !postId ||
      !(await ownZhihuArticle(page, postId, expectedTitle, {
        selfUrl,
        articlesOrigin,
        expectedAccountId,
      }))
    ) {
      return unknown("account_ownership_unverified", "readback");
    }
    const observedAt = new Date().toISOString();
    return {
      status: "completed",
      public_url: publicUrl,
      stage: "public_readback",
      occurred_at: observedAt,
      evidence: [
        {
          kind: "public_readback",
          url: publicUrl,
          observed_title: observedTitle,
          expected_sha256: digest(expected.title, expected.body),
          readback_sha256: digest(heading ?? "", body),
          content_matched: true,
          owned_by_account: true,
          observed_at: observedAt,
        },
      ],
      connector_version: CONNECTOR_VERSION,
    };
  } catch {
    return unknown("public_readback_unavailable", "readback");
  } finally {
    if (publicContext) await publicContext.close();
  }
}

export async function publishZhihu(
  page,
  payload,
  {
    editorUrl = ZHIHU_EDITOR,
    postOrigin = ZHIHU_POST_ORIGIN,
    selfUrl = ZHIHU_SELF,
    articlesOrigin = "https://www.zhihu.com",
    proxy,
    expectedAccountId,
  } = {},
) {
  const content = contentPayload(payload);
  if (!content) return unsupported("invalid_content_payload");
  await page.goto(editorUrl, {
    waitUntil: "domcontentloaded",
    timeout: 20_000,
  });
  if (/\/signin\b/.test(new URL(page.url()).pathname)) {
    return {
      status: "login_required",
      evidence: [],
      connector_version: CONNECTOR_VERSION,
    };
  }
  const title = page
    .locator(".WriteIndex-titleInput textarea, input[placeholder*='标题']")
    .first();
  const body = page
    .locator(".public-DraftEditor-content, .ProseMirror")
    .first();
  if (
    !(await title.isVisible().catch(() => false)) ||
    !(await body.isVisible().catch(() => false))
  ) {
    return unsupported("editor_selectors_unverified");
  }
  let stage = "draft";
  try {
    await title.fill(content.title);
    await body.fill(content.body);
    stage = "submit";
    await page
      .getByRole("button", { name: "发布", exact: true })
      .click({ timeout: 8_000 });
  } catch {
    return unknown("draft_or_submit_unverified", stage);
  }
  try {
    await page.waitForURL(
      (url) => zhihuPostUrl(url.href, postOrigin) !== null,
      {
        timeout: 10_000,
      },
    );
  } catch {
    return unknown("submission_outcome_unverified", "submit");
  }
  return readbackZhihu(page, page.url(), content, {
    postOrigin,
    selfUrl,
    articlesOrigin,
    proxy,
    expectedAccountId,
  });
}

function plainTextHtml(text) {
  return text
    .split(/\r?\n/u)
    .map(
      (line) =>
        `<p>${line.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;") || "<br>"}</p>`,
    )
    .join("");
}

export async function publishBaidu(
  page,
  payload,
  { editorUrl = BAIDU_EDITOR } = {},
) {
  const content = contentPayload(payload);
  if (!content) return unsupported("invalid_content_payload");
  await page.goto(editorUrl, {
    waitUntil: "domcontentloaded",
    timeout: 20_000,
  });
  if (/\/login\b/.test(new URL(page.url()).pathname)) {
    return {
      status: "login_required",
      evidence: [],
      connector_version: CONNECTOR_VERSION,
    };
  }
  const modernFields = page.locator(
    "div[class*='FeEditorApp-'][contenteditable='true']:visible",
  );
  const modern = (await modernFields.count()) === 2;
  const legacyTitle = page
    .locator(
      "[data-testid='news-title-input'], .title-input__inner, .input-box",
    )
    .first();
  const title = modern ? modernFields.nth(0) : legacyTitle;
  if (!(await title.isVisible().catch(() => false)))
    return unsupported("editor_selectors_unverified");
  let stage = "draft";
  try {
    await title.fill(content.title);
    let filled;
    if (modern) {
      await modernFields.nth(1).fill(content.body);
      filled = true;
    } else {
      // Static, developer-owned bridge to the page's single UEditor instance.
      // User text is escaped and serialized as data, never executable script.
      filled = await page.evaluate((html) => {
        const editors = Object.values(window.UE?.instants ?? {}).filter(
          (editor) => typeof editor?.setContent === "function",
        );
        if (editors.length !== 1) return false;
        editors[0].setContent(html);
        return true;
      }, plainTextHtml(content.body));
    }
    if (!filled) return unknown("body_editor_unverified", "draft");
    stage = "submit";
    await page
      .getByRole("button", { name: "发布", exact: true })
      .click({ timeout: 8_000 });
    const confirm = page.getByRole("button", { name: "确认发布", exact: true });
    if (await confirm.isVisible({ timeout: 2_000 }).catch(() => false)) {
      await confirm.click({ timeout: 8_000 });
    }
  } catch {
    return unknown("draft_or_submit_unverified", stage);
  }
  // "审核中" and toast success prove at most submission, not public content.
  return unknown("public_readback_unverified", "submit", [
    { kind: "submission_attempted" },
  ]);
}

export const adapters = Object.freeze({
  zhihu: Object.freeze({
    origin: "https://www.zhihu.com",
    entry: "https://www.zhihu.com/creator",
    operations: Object.freeze(["publish", "lookup"]),
    allowLoginControl(url) {
      return url.hostname === "www.zhihu.com" && url.pathname === "/signin";
    },
    identify(page) {
      return probeOwnAccount(page, ZHIHU_SELF, zhihuIdentity);
    },
    async execute(page, operation, payload, network) {
      if (operation === "publish") return publishZhihu(page, payload, network);
      if (operation === "lookup") {
        const content = contentPayload(payload, true);
        if (!content || typeof payload.public_url !== "string") {
          return unsupported("invalid_lookup_payload");
        }
        return readbackZhihu(page, payload.public_url, content, network);
      }
      return unsupported("operation_not_supported");
    },
  }),
  baidu_creator: Object.freeze({
    origin: "https://baijiahao.baidu.com",
    entry: "https://baijiahao.baidu.com/builder/theme/bjh/login",
    operations: Object.freeze(["publish", "lookup"]),
    allowLoginControl(url) {
      return (
        url.hostname === "baijiahao.baidu.com" &&
        url.pathname.startsWith("/builder/theme/bjh/login")
      );
    },
    identify(page) {
      return probeOwnAccount(page, BAIDU_SELF, baiduIdentity);
    },
    async execute(page, operation, payload) {
      if (operation === "publish") return publishBaidu(page, payload);
      return unsupported("public_lookup_unverified");
    },
  }),
  xiaohongshu: Object.freeze({
    origin: "https://creator.xiaohongshu.com",
    entry: "https://creator.xiaohongshu.com/login",
    operations: Object.freeze(["publish", "lookup"]),
    allowLoginControl(url) {
      return (
        url.hostname === "creator.xiaohongshu.com" && url.pathname === "/login"
      );
    },
    identify(page) {
      return xiaohongshuIdentity(page);
    },
    async execute() {
      return unsupported("media_or_article_flow_unverified");
    },
  }),
  kimi: Object.freeze({
    origin: "https://www.kimi.com",
    entry: "https://www.kimi.com/",
    operations: Object.freeze(["measure"]),
    allowLoginControl(url) {
      return url.hostname === "www.kimi.com" && url.pathname === "/";
    },
    identify(page) {
      return probeKimiAccount(page);
    },
    async execute() {
      // A generated answer, OAuth login, or candidate citations alone do not
      // establish an authenticated official web-search measurement.
      return unsupported("official_web_search_unverified");
    },
  }),
});
