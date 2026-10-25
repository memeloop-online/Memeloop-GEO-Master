// Fixed platform URLs and source-derived selectors. This connector version is
// not live-verified: no authenticated account was used to prove these flows.
import { createHash } from "node:crypto";
import { reportObservationDiagnostic } from "./ai-observation-parser.mjs";
import { observeDeepSeekWebSearch } from "./deepseek-web-search.mjs";
import { inspectDeepSeekMeasurementOptions } from "./deepseek-model-discovery.mjs";
import { capturedNewConversationId } from "./provider-conversation-ownership.mjs";
import { consumerWebProviders } from "./consumer-web-providers.mjs";
import { probeDeepseekAccount } from "./consumer-web-identity.mjs";
import { baiduCreatorAdapter } from "./providers/baidu-creator.mjs";
import { doubaoAdapter } from "./providers/doubao.mjs";
import { glmAdapter } from "./providers/glm.mjs";
import { kimiAdapter } from "./providers/kimi.mjs";
import { qianwenAdapter } from "./providers/qianwen.mjs";
import { xiaohongshuAdapter } from "./providers/xiaohongshu.mjs";
import { yuanbaoAdapter } from "./providers/yuanbao.mjs";
import {
  CONNECTOR_VERSION,
  completedMeasurement,
  contentPayload,
  digest,
  normalized,
  ownIdentity,
  probeOwnAccount,
  unknown,
  unsupported,
  validMeasurementPayload,
} from "./providers/contract.mjs";
export {
  glmIdentity,
  deepseekIdentity,
  doubaoIdentity,
  qianwenIdentity,
  probeGlmAccount,
  probeDeepseekAccount,
  probeDoubaoAccount,
  probeQianwenAccount,
} from "./consumer-web-identity.mjs";
export { CONNECTOR_VERSION, probeOwnAccount } from "./providers/contract.mjs";
export { baiduIdentity, publishBaidu } from "./providers/baidu-creator.mjs";
export {
  extractKimiCandidateObservation,
  kimiIdentity,
  measureKimi,
  observeKimiSearchAttempt,
  probeKimiAccount,
} from "./providers/kimi.mjs";
export { xiaohongshuIdentity } from "./providers/xiaohongshu.mjs";

const ZHIHU_SELF = "https://www.zhihu.com/api/v4/me";
const ZHIHU_EDITOR = "https://zhuanlan.zhihu.com/write";
const ZHIHU_POST_ORIGIN = "https://zhuanlan.zhihu.com";
const ZHIHU_ARTICLES_PAGE_SIZE = 20;
const ZHIHU_OWNERSHIP_MAX_PAGES = 10;

export function zhihuIdentity(data) {
  return ownIdentity(data?.id, data?.name, data?.avatar_url);
}

export async function measureDeepseek(
  page,
  payload,
  {
    expectedAccountId,
    deadlineAt,
    sourceDeadlineAt,
    signal,
    onEvidence,
    onConversationCaptured,
    apiExtract,
    getExtractionPolicy,
  } = {},
) {
  if (!validMeasurementPayload(payload, "deepseek"))
    return unsupported("invalid_measurement_payload");
  if (typeof expectedAccountId !== "string" || !expectedAccountId.trim())
    return unsupported("account_identity_unverified");
  if (!page) return unsupported("official_web_search_unverified");
  if (typeof onConversationCaptured !== "function")
    return unsupported("ownership_unavailable");
  if (typeof onEvidence !== "function")
    return unsupported("evidence_persistence_unavailable");
  const capturedEvidence = [];
  const ownershipEvidence = [];
  const diagnostics = [];
  const observation = await observeDeepSeekWebSearch(page, payload, {
    expectedAccountId,
    deadlineAt,
    sourceDeadlineAt,
    signal,
    apiExtract,
    getExtractionPolicy,
    onConversationCaptured,
    async persistConversation({
      createdSession,
      captured,
      binding,
      expectedAccountId: observedAccountId,
      signal: captureSignal,
    }) {
      const conversationId = capturedNewConversationId(captured, {
        provider: "deepseek",
      });
      const submission = captured.ownership?.submission;
      if (
        captureSignal?.aborted ||
        observedAccountId !== expectedAccountId ||
        createdSession?.source !== "browser_observed_create" ||
        !conversationId ||
        conversationId !== createdSession.chatSessionId ||
        submission?.model_type !== payload.model ||
        submission?.prompt_sha256 !== binding.questionSha256 ||
        binding.questionSha256 !==
          createHash("sha256").update(payload.question, "utf8").digest("hex") ||
        binding.model !== payload.model ||
        binding.parentMessageId !== null ||
        binding.searchEnabled !== true ||
        binding.thinkingEnabled !== false
      )
        return { durable: false };
      const receipt = {
        provider: "deepseek",
        purpose: "measurement",
        external_conversation_id: conversationId,
      };
      let ack;
      try {
        ack = await onConversationCaptured(receipt);
      } finally {
        ownershipEvidence.push({
          kind: "provider_conversation_ownership",
          schema_version: "geo.provider_conversation.ownership.v1",
          ...receipt,
          ownership_receipt_status:
            ack?.durable === true ? "persisted" : "unpersisted",
        });
      }
      // false acknowledges staging only: onEvidence commits the source and
      // ownership atomically before the interpreter may invoke extraction.
      return ack;
    },
    async onEvidence(record) {
      capturedEvidence.push(record);
      await onEvidence(record);
    },
    onDiagnostic(entry) {
      if (diagnostics.length >= 12) return;
      reportObservationDiagnostic(
        (safe) => diagnostics.push(safe),
        entry.stage,
        entry.code,
        entry.route,
      );
    },
  });
  if (observation?.reason === "requested_model_unavailable")
    return unsupported("requested_model_unavailable");
  if (
    !observation ||
    observation.reason ||
    typeof observation.raw_answer !== "string" ||
    !observation.raw_answer.trim() ||
    !Array.isArray(observation.citations) ||
    !observation.extraction_audit ||
    !observation.search_event
  )
    return unknown(
      observation?.reason ?? "official_search_observation_unverified",
      "measure",
      [...capturedEvidence, ...ownershipEvidence, ...diagnostics],
    );
  return completedMeasurement(payload, observation, [
    ...capturedEvidence,
    ...ownershipEvidence,
  ]);
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
  const observed = await publicZhihuArticle(page, publicUrl, expected, {
    postOrigin,
    proxy,
  });
  if (observed.status === "unknown") {
    return observed.reason === "public_title_mismatch"
      ? unknown("public_content_mismatch", "readback")
      : observed;
  }
  if (
    !(await ownZhihuArticle(page, observed.postId, observed.title, {
      selfUrl,
      articlesOrigin,
      expectedAccountId,
    }))
  ) {
    return unknown("account_ownership_unverified", "readback");
  }
  return completedZhihuReadback(publicUrl, expected, observed);
}

function completedZhihuReadback(publicUrl, expected, observed) {
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
        observed_title: observed.title,
        expected_sha256: digest(expected.title, expected.body),
        readback_sha256: observed.sha256,
        content_matched: true,
        owned_by_account: true,
        observed_at: observedAt,
      },
    ],
    connector_version: CONNECTOR_VERSION,
  };
}

async function publicZhihuArticle(
  page,
  publicUrl,
  expected,
  { postOrigin, proxy },
) {
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
    if (observedTitle !== expectedTitle) {
      return unknown("public_title_mismatch", "readback");
    }
    if (observedBody !== expectedBody) {
      return unknown("public_content_mismatch", "readback");
    }
    const postId = new URL(publicUrl).pathname.match(/^\/p\/(\d+)/u)?.[1];
    if (!postId) return unknown("public_url_unverified", "readback");
    return {
      postId,
      title: observedTitle,
      sha256: digest(heading ?? "", body),
    };
  } catch {
    return unknown("public_readback_unavailable", "readback");
  } finally {
    if (publicContext) await publicContext.close();
  }
}

// The list provides only IDs and titles. It is never evidence of content,
// chronology, or a causal link to the unknown submission.
async function discoverZhihuArticle(
  page,
  expected,
  {
    postOrigin = ZHIHU_POST_ORIGIN,
    selfUrl = ZHIHU_SELF,
    articlesOrigin = "https://www.zhihu.com",
    proxy,
    expectedAccountId,
  } = {},
) {
  if (
    typeof expectedAccountId !== "string" ||
    !expectedAccountId.trim() ||
    articlesOrigin !== new URL(articlesOrigin).origin ||
    new URL(selfUrl).origin !== articlesOrigin
  ) {
    return unknown("own_account_list_unverified", "discovery");
  }
  let ownerToken;
  const seen = new Set();
  const titleMatchedIds = [];
  let listComplete = false;
  for (
    let pageNumber = 0;
    pageNumber < ZHIHU_OWNERSHIP_MAX_PAGES;
    pageNumber++
  ) {
    const self = await probeOwnAccount(page, selfUrl, (data) => data);
    const identity = zhihuIdentity(self);
    const token = self?.url_token;
    if (
      identity?.platform_account_id !== expectedAccountId ||
      typeof token !== "string" ||
      !/^[\w-]{1,128}$/u.test(token) ||
      (ownerToken !== undefined && token !== ownerToken)
    ) {
      return unknown("own_account_list_unverified", "discovery");
    }
    ownerToken = token;
    const endpoint = `${articlesOrigin}/api/v4/members/${encodeURIComponent(token)}/articles?limit=${ZHIHU_ARTICLES_PAGE_SIZE}&offset=${pageNumber * ZHIHU_ARTICLES_PAGE_SIZE}`;
    const result = await probeOwnAccount(page, endpoint, (data) => data);
    const articles = result?.data;
    const isEnd = result?.paging?.is_end;
    if (
      !Array.isArray(articles) ||
      articles.length > ZHIHU_ARTICLES_PAGE_SIZE ||
      typeof isEnd !== "boolean" ||
      (!isEnd && articles.length !== ZHIHU_ARTICLES_PAGE_SIZE)
    ) {
      return unknown("own_account_list_unverified", "discovery");
    }
    for (const article of articles) {
      const id = article?.id;
      const postId =
        typeof id === "number" && Number.isSafeInteger(id) && id > 0
          ? String(id)
          : typeof id === "string" && /^[1-9]\d*$/u.test(id)
            ? id
            : null;
      if (
        !postId ||
        typeof article?.title !== "string" ||
        !article.title.trim() ||
        seen.has(postId)
      ) {
        return unknown("own_account_list_unverified", "discovery");
      }
      seen.add(postId);
      if (normalized(article.title) === normalized(expected.title)) {
        titleMatchedIds.push(postId);
      }
    }
    if (isEnd) {
      listComplete = true;
      break;
    }
  }
  if (!listComplete) return unknown("own_account_list_unverified", "discovery");

  let match;
  for (const id of titleMatchedIds) {
    const publicUrl = zhihuPublicationCandidateUrl(
      `${postOrigin}/p/${id}`,
      postOrigin,
    );
    if (!publicUrl) return unknown("public_url_unverified", "discovery");
    const observed = await publicZhihuArticle(page, publicUrl, expected, {
      postOrigin,
      proxy,
    });
    if (observed.status === "unknown") {
      // A title-matched asset which cannot be read leaves the result
      // ambiguous even when a different candidate matched exactly.
      if (observed.reason === "public_content_mismatch") continue;
      return unknown(observed.reason, "discovery");
    }
    if (match) return unknown("own_account_list_ambiguous", "discovery");
    match = { publicUrl, observed };
  }
  if (!match) return unknown("own_account_list_no_match", "discovery");
  const completed = completedZhihuReadback(
    match.publicUrl,
    expected,
    match.observed,
  );
  completed.evidence.push({
    kind: "own_account_list_discovery",
    schema_version: "geo.publication.discovery.v1",
    public_url: match.publicUrl,
    expected_sha256: digest(expected.title, expected.body),
    list_complete: true,
    exact_match_count: 1,
    observed_at: completed.occurred_at,
  });
  return completed;
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

export const adapters = Object.freeze({
  zhihu: Object.freeze({
    loginEntryAvailable: true,
    loginSupported: true,
    connectorVersion: CONNECTOR_VERSION,
    origin: "https://www.zhihu.com",
    entry: "https://www.zhihu.com/creator",
    operations: Object.freeze(["publish", "lookup"]),
    identify(page) {
      return probeOwnAccount(page, ZHIHU_SELF, zhihuIdentity);
    },
    async execute(page, operation, payload, network) {
      if (operation === "publish") return publishZhihu(page, payload, network);
      if (operation === "lookup") {
        const content = contentPayload(payload, true);
        if (
          !content ||
          (payload.public_url !== undefined &&
            typeof payload.public_url !== "string")
        ) {
          return unsupported("invalid_lookup_payload");
        }
        return payload.public_url === undefined
          ? discoverZhihuArticle(page, content, network)
          : readbackZhihu(page, payload.public_url, content, network);
      }
      return unsupported("operation_not_supported");
    },
  }),
  baidu_creator: baiduCreatorAdapter,
  xiaohongshu: xiaohongshuAdapter,
  kimi: kimiAdapter,
  glm: glmAdapter,
  deepseek: Object.freeze({
    ...consumerWebProviders.deepseek,
    inspectMeasurementOptions: inspectDeepSeekMeasurementOptions,
    connectorVersion: CONNECTOR_VERSION,
    operations: Object.freeze(["measure"]),
    identify: probeDeepseekAccount,
    async execute(page, operation, payload, network) {
      if (operation !== "measure")
        return unsupported("operation_not_supported");
      return measureDeepseek(page, payload, network);
    },
  }),
  doubao: doubaoAdapter,
  qianwen: qianwenAdapter,
  yuanbao: yuanbaoAdapter,
});
