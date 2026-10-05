// Fixed platform URLs and source-derived selectors. This connector version is
// not live-verified: no authenticated account was used to prove these flows.
import { createHash } from "node:crypto";
import { observeKimiConnectSearch } from "./kimi-connect-search.mjs";

export const CONNECTOR_VERSION = "live_unverified.source_derived.v1";

const ZHIHU_SELF = "https://www.zhihu.com/api/v4/me";
const BAIDU_SELF = "https://baijiahao.baidu.com/builder/app/appinfo";
const ZHIHU_EDITOR = "https://zhuanlan.zhihu.com/write";
const BAIDU_EDITOR = "https://baijiahao.baidu.com/builder/rc/edit?type=news";
const ZHIHU_POST_ORIGIN = "https://zhuanlan.zhihu.com";
const ZHIHU_ARTICLES_PAGE_SIZE = 20;
const ZHIHU_OWNERSHIP_MAX_PAGES = 10;
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

// A temporary Chromium page retains its assigned BrowserContext cookies and
// proxy. Intercept its network request/response before a redirect is followed;
// Playwright route.fetch would use a separate transport, bypassing that proxy.
export async function probeOwnAccount(page, endpoint, extract) {
  const probe = await page.context().newPage();
  let session;
  let onPaused;
  try {
    const expected = new URL(endpoint);
    session = await page.context().newCDPSession(probe);
    onPaused = (event) => {
      const allowed =
        event.request.url === expected.href &&
        (event.responseStatusCode === undefined ||
          event.responseStatusCode === 200);
      void session
        .send(
          allowed ? "Fetch.continueRequest" : "Fetch.failRequest",
          allowed
            ? { requestId: event.requestId }
            : { requestId: event.requestId, errorReason: "BlockedByClient" },
        )
        .catch(() => {});
    };
    session.on("Fetch.requestPaused", onPaused);
    await session.send("Fetch.enable", {
      patterns: [
        { urlPattern: "*", requestStage: "Request" },
        { urlPattern: "*", requestStage: "Response" },
      ],
    });
    const response = await probe.goto(endpoint, {
      waitUntil: "domcontentloaded",
      timeout: 12_000,
    });
    if (!response || response.status() !== 200) return null;
    const actual = new URL(probe.url());
    if (actual.href !== expected.href) return null;
    if (!/application\/json/i.test(response.headers()["content-type"] ?? ""))
      return null;
    const text = await response.text();
    if (text.length > 128_000) return null;
    return extract(JSON.parse(text));
  } catch {
    return null;
  } finally {
    // Closing a stalled navigation first releases the browser's pending
    // network request; otherwise disabling interception can wait indefinitely.
    await probe.close({ timeout: 2_000 }).catch(() => {});
    if (session) {
      if (onPaused) session.off("Fetch.requestPaused", onPaused);
      await session.send("Fetch.disable").catch(() => {});
      await session.detach().catch(() => {});
    }
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

// A live connector must supply the UI action, provider-event decoder and
// answer reader only after an authenticated browser trace establishes their
// actual semantics. This captures *candidates*, never a verified receipt.
// In particular, a same-origin response or a request ID from the answer is
// not by itself proof that the provider performed official web search.
export async function observeKimiSearchAttempt(
  page,
  question,
  {
    submit,
    decodeEvent,
    readAnswer,
    waitForCompletion,
    timeoutMs = 30_000,
    trustedOrigin = KIMI_ORIGIN,
  },
) {
  if (
    typeof question !== "string" ||
    !question.trim() ||
    typeof submit !== "function" ||
    typeof decodeEvent !== "function" ||
    typeof readAnswer !== "function" ||
    (waitForCompletion !== undefined &&
      typeof waitForCompletion !== "function") ||
    !Number.isFinite(timeoutMs) ||
    timeoutMs <= 0
  ) {
    return null;
  }
  const events = [];
  const pending = new Set();
  let closed = false;
  let timer;
  const controller = new AbortController();
  const onResponse = (response) => {
    const task = (async () => {
      try {
        const url = new URL(response.url());
        if (
          url.origin !== trustedOrigin ||
          response.status() !== 200 ||
          !/application\/json/i.test(response.headers()["content-type"] ?? "")
        ) {
          return;
        }
        const body = await response.body();
        if (body.length > 128_000) return;
        const event = decodeEvent(JSON.parse(body.toString("utf8")), response);
        if (
          !closed &&
          event &&
          typeof event.event_id === "string" &&
          /^[\w-]{1,128}$/u.test(event.event_id) &&
          typeof event.request_id === "string" &&
          /^[\w-]{1,128}$/u.test(event.request_id)
        ) {
          events.push({
            event_id: event.event_id,
            request_id: event.request_id,
            // Local observation time is not the provider's event timestamp.
            received_at: new Date().toISOString(),
          });
        }
      } catch {
        // Malformed and unrelated browser traffic is not search evidence.
      }
    })();
    pending.add(task);
    void task.finally(() => pending.delete(task));
  };
  page.on("response", onResponse);
  try {
    return await Promise.race([
      (async () => {
        await submit(page, question, controller.signal);
        if (closed) return null;
        const answer = extractKimiCandidateObservation(
          await readAnswer(page, controller.signal),
        );
        if (closed || !answer?.request_id) return null;
        // The adapter must identify provider completion, not infer it from
        // answer rendering or an arbitrary quiet period. This remains only
        // candidate collection, never proof of official search.
        if (waitForCompletion) {
          await waitForCompletion(page, answer.request_id, controller.signal);
        }
        if (closed) return null;
        while (pending.size && !closed) {
          await Promise.allSettled([...pending]);
        }
        if (closed) return null;
        const matched = events.filter(
          (event) => event.request_id === answer.request_id,
        );
        if (matched.length !== 1) return null;
        return { ...answer, candidate_search_event: matched[0] };
      })(),
      new Promise((resolve) => {
        timer = setTimeout(() => resolve(null), timeoutMs);
      }),
    ]);
  } catch {
    return null;
  } finally {
    closed = true;
    clearTimeout(timer);
    controller.abort();
    page.off("response", onResponse);
  }
}

function validKimiMeasurementPayload(payload) {
  return (
    payload !== null &&
    typeof payload === "object" &&
    !Array.isArray(payload) &&
    Object.keys(payload).every((key) =>
      [
        "target_id",
        "account_id",
        "provider",
        "model",
        "surface",
        "search_mode",
        "protocol_version",
        "question_set_version",
        "question",
        "market",
        "language",
        "scheduled_at",
        "sample_ordinal",
      ].includes(key),
    ) &&
    ["target_id", "account_id"].every(
      (key) =>
        typeof payload[key] === "string" &&
        /^[\da-f]{8}-[\da-f]{4}-[\da-f]{4}-[\da-f]{4}-[\da-f]{12}$/iu.test(
          payload[key],
        ),
    ) &&
    payload.provider === "kimi" &&
    payload.surface === "consumer_web" &&
    payload.search_mode === "web_search" &&
    [
      "model",
      "protocol_version",
      "question_set_version",
      "question",
      "market",
      "language",
    ].every(
      (key) =>
        typeof payload[key] === "string" &&
        payload[key].trim().length > 0 &&
        payload[key].length <= 4096,
    ) &&
    typeof payload.scheduled_at === "string" &&
    Number.isFinite(Date.parse(payload.scheduled_at)) &&
    Number.isInteger(payload.sample_ordinal) &&
    payload.sample_ordinal >= 0
  );
}

export async function measureKimi(
  page,
  payload,
  { searchFlow, expectedAccountId } = {},
) {
  if (!validKimiMeasurementPayload(payload))
    return unsupported("invalid_measurement_payload");
  if (typeof expectedAccountId !== "string" || !expectedAccountId.trim())
    return unsupported("account_identity_unverified");
  // Historical injected JSON candidates never confer official search proof.
  if (searchFlow) {
    const candidate = await observeKimiSearchAttempt(page, payload.question, {
      ...searchFlow,
    });
    if (!candidate)
      return unknown("official_search_event_unverified", "measure");
    return unknown("official_search_provenance_unverified", "measure");
  }
  if (!page) return unsupported("official_web_search_unverified");
  const observation = await observeKimiConnectSearch(page, payload);
  if (observation?.reason === "requested_model_unavailable")
    return unsupported("requested_model_unavailable");
  if (!observation)
    return unknown("official_search_observation_unverified", "measure");
  const observedAt = new Date().toISOString();
  const rawAnswer = observation.raw_answer;
  return {
    status: "completed",
    stage: "official_search_observation",
    occurred_at: observedAt,
    raw_answer: rawAnswer,
    evidence: [
      {
        kind: "official_search_observation",
        schema_version: "geo.measure.official_search.v2",
        target_id: payload.target_id,
        account_id: payload.account_id,
        provider: payload.provider,
        model: payload.model,
        surface: payload.surface,
        search_mode: payload.search_mode,
        protocol_version: payload.protocol_version,
        question_set_version: payload.question_set_version,
        question_sha256: createHash("sha256")
          .update(payload.question, "utf8")
          .digest("hex"),
        market: payload.market,
        language: payload.language,
        scheduled_at: payload.scheduled_at,
        sample_ordinal: payload.sample_ordinal,
        connector_version: CONNECTOR_VERSION,
        provenance: "live",
        disposition: "observed",
        raw_answer: rawAnswer,
        citations: observation.citations,
        search_event: observation.search_event,
      },
    ],
    connector_version: CONNECTOR_VERSION,
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

function zhihuPublicationCandidateUrl(value, postOrigin) {
  try {
    const expected = new URL(postOrigin);
    const loopback =
      expected.protocol === "http:" &&
      (expected.hostname === "127.0.0.1" ||
        expected.hostname === "localhost" ||
        expected.hostname === "[::1]");
    if (
      postOrigin !== expected.origin ||
      (postOrigin !== ZHIHU_POST_ORIGIN && !loopback)
    ) {
      return null;
    }
    const url = new URL(value);
    const canonical = `${expected.origin}${url.pathname}`;
    return url.origin === expected.origin &&
      !url.username &&
      !url.password &&
      !url.search &&
      !url.hash &&
      /^\/p\/[0-9]+$/u.test(url.pathname) &&
      url.href === canonical
      ? canonical
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
  // Only the configured account's fixed list endpoint is queried. Never
  // navigate to an API-provided paging.next URL or search another account.
  if (
    articlesOrigin !== new URL(articlesOrigin).origin ||
    new URL(selfUrl).origin !== articlesOrigin
  )
    return false;
  let ownerId;
  let ownerToken;
  for (
    let pageNumber = 0;
    pageNumber < ZHIHU_OWNERSHIP_MAX_PAGES;
    pageNumber++
  ) {
    const self = await probeOwnAccount(page, selfUrl, (data) => data);
    const identity = zhihuIdentity(self);
    const token = self?.url_token;
    if (
      !identity ||
      (expectedAccountId !== undefined &&
        identity.platform_account_id !== expectedAccountId) ||
      (ownerId !== undefined && identity.platform_account_id !== ownerId) ||
      typeof token !== "string" ||
      !/^[\w-]{1,128}$/u.test(token) ||
      (ownerToken !== undefined && token !== ownerToken)
    ) {
      return false;
    }
    ownerId = identity.platform_account_id;
    ownerToken = token;
    const endpoint = `${articlesOrigin}/api/v4/members/${encodeURIComponent(token)}/articles?limit=${ZHIHU_ARTICLES_PAGE_SIZE}&offset=${pageNumber * ZHIHU_ARTICLES_PAGE_SIZE}`;
    const result = await probeOwnAccount(page, endpoint, (data) => data);
    const articles = result?.data;
    if (!Array.isArray(articles) || articles.length > ZHIHU_ARTICLES_PAGE_SIZE)
      return false;
    if (
      articles.some(
        (article) =>
          String(article?.id) === postId &&
          typeof article?.title === "string" &&
          normalized(article.title) === title,
      )
    ) {
      return true;
    }
    const isEnd = result?.paging?.is_end;
    if (result?.paging !== undefined && typeof isEnd !== "boolean")
      return false;
    if (isEnd === true || articles.length === 0) return false;
    if (isEnd === undefined && articles.length < ZHIHU_ARTICLES_PAGE_SIZE)
      return false;
  }
  return false;
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
  const candidateUrl = zhihuPublicationCandidateUrl(page.url(), postOrigin);
  const observedAt = new Date().toISOString();
  const outcome = await readbackZhihu(page, page.url(), content, {
    postOrigin,
    selfUrl,
    articlesOrigin,
    proxy,
    expectedAccountId,
  });
  if (outcome.status !== "unknown" || !candidateUrl) return outcome;
  return {
    ...outcome,
    occurred_at: observedAt,
    evidence: [
      ...outcome.evidence,
      {
        kind: "publication_candidate",
        schema_version: "geo.publication.candidate.v1",
        url: candidateUrl,
        expected_sha256: digest(content.title, content.body),
        observed_at: observedAt,
        source: "post_submit_navigation",
      },
    ],
  };
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
    connectorVersion: CONNECTOR_VERSION,
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
    connectorVersion: CONNECTOR_VERSION,
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
    connectorVersion: CONNECTOR_VERSION,
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
    connectorVersion: CONNECTOR_VERSION,
    origin: "https://www.kimi.com",
    entry: "https://www.kimi.com/",
    operations: Object.freeze(["measure"]),
    allowLoginControl(url) {
      return url.hostname === "www.kimi.com" && url.pathname === "/";
    },
    identify(page) {
      return probeKimiAccount(page);
    },
    async execute(page, operation, payload, network) {
      if (operation !== "measure")
        return unsupported("operation_not_supported");
      return measureKimi(page, payload, network);
    },
  }),
});
