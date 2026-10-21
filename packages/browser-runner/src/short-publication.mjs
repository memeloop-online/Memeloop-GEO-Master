import { createHash } from "node:crypto";
import { probeOwnAccount, zhihuIdentity } from "./adapters.mjs";

export const SHORT_PUBLICATION_VERSION = "zhihu.short_post.ui.v1";
const ORIGIN = "https://www.zhihu.com";
const FORM = ".WritePinV2-Form";
const EDITOR = '.public-DraftEditor-content[contenteditable="true"]';
const MENU = ".Popover-content.PinToolbar-menuContainer:visible";
const DELETE_TITLE = "你确定要删除自己的想法吗？";

function normalized(text) {
  return text.replace(/\s+/gu, " ").trim();
}

function hash(body) {
  return createHash("sha256")
    .update(`\n${normalized(body)}`, "utf8")
    .digest("hex");
}

function result(status, reason, stage, evidence = []) {
  return {
    status,
    ...(reason ? { reason } : {}),
    stage,
    evidence,
    connector_version: SHORT_PUBLICATION_VERSION,
  };
}

function content(payload, exactId = false) {
  const fields = exactId ? ["external_id", "body"] : ["title", "body"];
  if (
    !payload ||
    typeof payload !== "object" ||
    Array.isArray(payload) ||
    Object.keys(payload).some((key) => !fields.includes(key)) ||
    typeof payload.body !== "string" ||
    !payload.body.trim() ||
    payload.body.length > 100_000 ||
    (!exactId &&
      payload.title !== undefined &&
      typeof payload.title !== "string") ||
    (exactId &&
      (typeof payload.external_id !== "string" ||
        !/^[1-9][0-9]*$/u.test(payload.external_id)))
  ) {
    return null;
  }
  return { body: payload.body, external_id: payload.external_id };
}

function settings(network = {}) {
  try {
    const origin = new URL(network.postOrigin ?? ORIGIN);
    const loopback =
      origin.protocol === "http:" &&
      ["127.0.0.1", "localhost", "[::1]"].includes(origin.hostname);
    if (
      origin.href !== `${origin.origin}/` ||
      (origin.origin !== ORIGIN && !loopback)
    ) {
      return null;
    }
    const selfUrl = network.selfUrl ?? `${origin.origin}/api/v4/me`;
    if (new URL(selfUrl).origin !== origin.origin) return null;
    return {
      origin: origin.origin,
      selfUrl,
      proxy: network.proxy,
      expectedAccountId: network.expectedAccountId,
      timeout: network.timeoutMs ?? 10_000,
    };
  } catch {
    return null;
  }
}

function pinUrl(value, origin) {
  try {
    const url = new URL(value, origin);
    return url.origin === origin &&
      !url.search &&
      !url.hash &&
      !url.username &&
      !url.password &&
      /^\/pin\/[1-9][0-9]*$/u.test(url.pathname)
      ? `${origin}${url.pathname}`
      : null;
  } catch {
    return null;
  }
}

async function owner(page, config) {
  const self = await probeOwnAccount(page, config.selfUrl, (data) => data);
  const identity = zhihuIdentity(self);
  if (
    !identity ||
    (config.expectedAccountId !== undefined &&
      identity.platform_account_id !== config.expectedAccountId) ||
    typeof self.url_token !== "string" ||
    !/^[\w-]{1,128}$/u.test(self.url_token)
  ) {
    return null;
  }
  return { id: identity.platform_account_id, token: self.url_token };
}

function sameOwner(first, second) {
  return (
    first && second && first.id === second.id && first.token === second.token
  );
}

function exactFields(value, fields) {
  return (
    value !== null &&
    typeof value === "object" &&
    !Array.isArray(value) &&
    Object.keys(value).length === fields.length &&
    fields.every((field) => Object.hasOwn(value, field))
  );
}

async function freshShortPostRequest(page, raw, expected) {
  try {
    if (typeof raw !== "string" || !raw || raw.length > 600_000) return false;
    const payload = JSON.parse(raw);
    const data = payload.data;
    // This endpoint also edits and schedules content. Only the observed fresh,
    // body-only composer request can attest to a new publication.
    if (
      !exactFields(payload, ["action", "data"]) ||
      payload.action !== "pin" ||
      !exactFields(data, [
        "publish",
        "commentsPermission",
        "extra_info",
        "draft",
        "hybrid",
      ]) ||
      !exactFields(data.draft, ["disabled"]) ||
      data.draft.disabled !== 1 ||
      !exactFields(data.extra_info, ["view_permission", "publisher"]) ||
      data.extra_info.view_permission !== "all" ||
      data.extra_info.publisher !== "pc" ||
      !exactFields(data.publish, ["traceId"]) ||
      typeof data.publish.traceId !== "string" ||
      !data.publish.traceId ||
      data.publish.traceId.length > 128 ||
      !exactFields(data.commentsPermission, ["comment_permission"]) ||
      typeof data.commentsPermission.comment_permission !== "string" ||
      !exactFields(data.hybrid, ["html", "textLength"]) ||
      typeof data.hybrid.html !== "string" ||
      !data.hybrid.html ||
      !Number.isSafeInteger(data.hybrid.textLength) ||
      data.hybrid.textLength < 1
    )
      return false;
    const text = await page.evaluate((html) => {
      const document = new DOMParser().parseFromString(html, "text/html");
      const body = document.body;
      if (
        document.head.childNodes.length ||
        [...body.querySelectorAll("*")].some(
          (node) => !["P", "DIV", "SPAN", "BR"].includes(node.tagName),
        )
      )
        return null;
      for (const br of body.querySelectorAll("br")) br.replaceWith("\n");
      for (const block of body.querySelectorAll("p,div")) block.append("\n");
      return body.textContent;
    }, data.hybrid.html);
    return (
      typeof text === "string" && normalized(text) === normalized(expected.body)
    );
  } catch {
    return false;
  }
}

// The observed text region excludes the item's timestamp, media and toolbar.
// Truncated text or multiple body regions leave the result unknown.
async function item(page, url, expected, account, config) {
  if (pinUrl(page.url(), config.origin) !== url) return null;
  const cards = page.locator(".PinItem:visible");
  await cards
    .locator(".RichContent-inner .RichText:visible")
    .first()
    .waitFor({ state: "visible", timeout: config.timeout });
  if ((await cards.count()) !== 1) return null;
  const card = cards.first();
  const links = await card
    .locator("a.ContentItem-time[href]")
    .evaluateAll((nodes) => nodes.map((node) => node.href));
  if (
    !links.length ||
    links.some((href) => pinUrl(href, config.origin) !== url)
  )
    return null;
  const authors = card.locator(".PinItem-authorInfo a.UserLink-link[href]");
  const hrefs = await authors.evaluateAll((links) =>
    links.map((link) => link.href),
  );
  if (
    !hrefs.length ||
    hrefs.some((href) => {
      const author = new URL(href);
      return (
        author.origin !== config.origin ||
        author.pathname !== `/people/${account.token}` ||
        author.search ||
        author.hash
      );
    })
  ) {
    return null;
  }
  const bodies = card.locator(".RichContent-inner .RichText:visible");
  if ((await bodies.count()) !== 1) return null;
  const remainder = card.locator(".PinItem-remainContentRichText");
  if (
    await remainder.evaluateAll((nodes) =>
      nodes.some(
        (node) =>
          node.textContent.trim() ||
          node.querySelector("img,video,audio,iframe"),
      ),
    )
  )
    return null;
  if (normalized(await bodies.innerText()) !== normalized(expected.body))
    return null;
  return card;
}

async function readback(page, expected, config, account) {
  const url = `${config.origin}/pin/${expected.external_id}`;
  let publicContext;
  try {
    // Public verification has no account cookies and keeps the assigned proxy.
    publicContext = await page
      .context()
      .browser()
      .newContext(config.proxy ? { proxy: config.proxy } : {});
    const publicPage = await publicContext.newPage();
    const response = await publicPage.goto(url, {
      waitUntil: "domcontentloaded",
      timeout: config.timeout,
    });
    if (
      response?.status() !== 200 ||
      !(await item(publicPage, url, expected, account, config))
    ) {
      return result(
        "unknown",
        "short_post_public_readback_unverified",
        "readback",
      );
    }
    if (!sameOwner(account, await owner(page, config))) {
      return result("unknown", "account_ownership_unverified", "readback");
    }
    const observedAt = new Date().toISOString();
    return {
      ...result("completed", null, "public_readback", [
        {
          kind: "public_readback",
          url,
          external_id: expected.external_id,
          placement_slot: "short_post",
          expected_sha256: hash(expected.body),
          readback_sha256: hash(expected.body),
          content_matched: true,
          owned_by_account: true,
          observed_at: observedAt,
        },
      ]),
      public_url: url,
      external_id: expected.external_id,
      occurred_at: observedAt,
    };
  } catch {
    return result(
      "unknown",
      "short_post_public_readback_unverified",
      "readback",
    );
  } finally {
    await publicContext?.close();
  }
}

export async function lookupShortPost(page, payload, network = {}) {
  const expected = content(payload, true);
  const config = settings(network);
  if (!expected || !config)
    return result("unsupported", "invalid_content_payload", "readback");
  const account = await owner(page, config);
  if (!account)
    return result("unknown", "account_ownership_unverified", "readback");
  return readback(page, expected, config, account);
}

export async function publishShortPost(page, payload, network = {}) {
  const expected = content(payload);
  const config = settings(network);
  if (!expected || !config)
    return result("unsupported", "invalid_content_payload", "draft");
  if (payload.title?.trim())
    return result("unsupported", "short_post_title_unverified", "draft");
  let stage = "draft";
  let submitted = false;
  let captureRequest;
  let captureSession;
  let requestBlocked = false;
  let forwardedRequestId;
  try {
    const account = await owner(page, config);
    if (!account)
      return result("unknown", "account_ownership_unverified", stage);
    await page.goto(`${config.origin}/`, {
      waitUntil: "domcontentloaded",
      timeout: config.timeout,
    });
    if (new URL(page.url()).pathname === "/signin")
      return result("login_required", null, stage);
    if (new URL(page.url()).origin !== config.origin)
      return result("unsupported", "editor_selectors_unverified", stage);
    await page
      .getByRole("button", { name: "发想法", exact: true })
      .click({ timeout: config.timeout });
    const form = page.locator(`${FORM}:visible`);
    const editor = form.locator(EDITOR);
    // Draft.js mounts asynchronously. Playwright waits for the actual editable.
    await editor.waitFor({ state: "visible", timeout: config.timeout });
    if ((await form.count()) !== 1 || (await editor.count()) !== 1)
      return result("unsupported", "editor_selectors_unverified", stage);
    const title = form.locator(".TitleArea textarea");
    if (
      ((await title.count()) === 1 && (await title.inputValue()).trim()) ||
      (await editor.innerText()).trim()
    )
      return result("unsupported", "short_post_existing_draft", stage);
    await editor.fill(expected.body, { timeout: config.timeout });
    if (!sameOwner(account, await owner(page, config)))
      return result("unknown", "account_ownership_unverified", stage);
    const submit = page
      .locator(".WritePinToolbar:visible")
      .getByRole("button", { name: "发布", exact: true });
    if ((await submit.count()) !== 1)
      return result("unsupported", "editor_submit_unverified", stage);
    stage = "submit";
    const createUrl = `${config.origin}/api/v4/content/publish`;
    let createRequest;
    captureRequest = (request) => {
      if (
        request.method() === "POST" &&
        request.url() === createUrl &&
        request.frame() === page.mainFrame()
      ) {
        createRequest ??= request;
      }
    };
    page.on("request", captureRequest);
    let capturedBody;
    let responseCount = 0;
    let requestAttempts = 0;
    // Keep the original browser transport, cookies and proxy. Validate the
    // first request before forwarding, then retain its bounded response before
    // navigation can discard the resource. No request is replayed.
    try {
      captureSession = await page.context().newCDPSession(page);
      const session = captureSession;
      const { frameTree } = await session.send("Page.getFrameTree");
      const capturePaused = (event) => {
        void (async () => {
          let bodyTimeout;
          let resume = true;
          try {
            const matches =
              event.frameId === frameTree.frame.id &&
              event.request.url === createUrl &&
              event.request.method === "POST";
            if (event.responseStatusCode === undefined) {
              requestAttempts++;
              if (
                !matches ||
                requestAttempts !== 1 ||
                forwardedRequestId ||
                event.redirectedRequestId ||
                !(await freshShortPostRequest(
                  page,
                  event.request.postData,
                  expected,
                ))
              ) {
                requestBlocked = true;
                resume = false;
                await session.send("Fetch.failRequest", {
                  requestId: event.requestId,
                  errorReason: "BlockedByClient",
                });
              } else {
                forwardedRequestId = event.requestId;
              }
              return;
            }
            if (!matches || event.requestId !== forwardedRequestId) return;
            responseCount++;
            if (
              responseCount !== 1 ||
              event.responseStatusCode < 200 ||
              event.responseStatusCode >= 300 ||
              event.responseHeaders?.some(
                (header) =>
                  header.name.toLowerCase() === "content-length" &&
                  Number(header.value) > 1_000_000,
              )
            )
              return;
            const body = await Promise.race([
              session.send("Fetch.getResponseBody", {
                requestId: event.requestId,
              }),
              new Promise((_, reject) => {
                bodyTimeout = setTimeout(
                  () => reject(new Error()),
                  config.timeout,
                );
              }),
            ]);
            if (body.body.length <= 1_400_000) {
              const bytes = Buffer.from(
                body.body,
                body.base64Encoded ? "base64" : "utf8",
              );
              if (bytes.length <= 1_000_000) capturedBody = bytes;
            }
          } catch {
            // An unavailable capture cannot grant creation authority.
          } finally {
            clearTimeout(bodyTimeout);
            if (resume)
              await session
                .send("Fetch.continueRequest", {
                  requestId: event.requestId,
                })
                .catch(() => {});
          }
        })();
      };
      captureSession.on("Fetch.requestPaused", capturePaused);
      await captureSession.send("Fetch.enable", {
        patterns: [
          { urlPattern: createUrl, requestStage: "Request" },
          { urlPattern: createUrl, requestStage: "Response" },
        ],
      });
    } catch {
      await captureSession?.detach().catch(() => {});
      captureSession = undefined;
    }
    if (!captureSession)
      return result(
        "unsupported",
        "short_post_create_capture_unavailable",
        stage,
      );
    const pendingReceipt = page
      .waitForResponse((response) => response.request() === createRequest, {
        timeout: config.timeout,
      })
      .catch(() => null);
    // There is exactly one click. Any exception after this point is unknown.
    submitted = true;
    await submit.click({ timeout: config.timeout });
    const receipt = await pendingReceipt;
    if (requestBlocked && !forwardedRequestId)
      return result(
        "unsupported",
        "short_post_create_request_unverified",
        stage,
      );
    const creation = [];
    if (
      capturedBody &&
      responseCount === 1 &&
      forwardedRequestId &&
      receipt?.ok() &&
      receipt.url() === createUrl &&
      !createRequest.redirectedFrom() &&
      !createRequest.redirectedTo()
    ) {
      // The response decoder is intentionally tied to the observed client
      // envelope; navigation and public readback cannot mint creation proof.
      try {
        const responseData = JSON.parse(capturedBody.toString("utf8"));
        const encoded = responseData?.data?.result;
        const created =
          typeof encoded === "string" ? JSON.parse(encoded) : null;
        if (
          typeof created?.id === "string" &&
          /^[1-9][0-9]{0,39}$/u.test(created.id) &&
          created.author?.id === account.id &&
          responseData.data.write?.instruction !== true &&
          responseCount === 1
        ) {
          creation.push({
            kind: "publication_created",
            schema_version: "geo.publication.created.v1",
            external_id: created.id,
            url: `${config.origin}/pin/${created.id}`,
            expected_sha256: hash(expected.body),
            request_method: "POST",
            request_url: createUrl,
            http_status: receipt.status(),
            observed_at: new Date().toISOString(),
          });
        }
      } catch {
        // An unreadable response preserves the read-only navigation candidate.
      }
    }
    if (!creation.length)
      await page.waitForURL((url) => pinUrl(url.href, config.origin) !== null, {
        timeout: config.timeout,
      });
    const externalId =
      creation[0]?.external_id ??
      new URL(pinUrl(page.url(), config.origin)).pathname.slice("/pin/".length);
    const outcome = await readback(
      page,
      { ...expected, external_id: externalId },
      config,
      account,
    );
    const navigationUrl = pinUrl(page.url(), config.origin);
    const evidence = navigationUrl
      ? [
          {
            kind: "publication_candidate",
            schema_version: "geo.publication.candidate.v1",
            url: navigationUrl,
            expected_sha256: hash(expected.body),
            observed_at: new Date().toISOString(),
            source: "post_submit_navigation",
          },
        ]
      : [];
    return {
      ...outcome,
      external_id: externalId,
      public_url: `${config.origin}/pin/${externalId}`,
      evidence: [...evidence, ...outcome.evidence, ...creation],
    };
  } catch {
    if (requestBlocked && !forwardedRequestId)
      return result(
        "unsupported",
        "short_post_create_request_unverified",
        stage,
      );
    return result(
      submitted ? "unknown" : "unsupported",
      submitted
        ? "submission_outcome_unverified"
        : "editor_selectors_unverified",
      stage,
    );
  } finally {
    if (captureRequest) page.off("request", captureRequest);
    await captureSession?.detach().catch(() => {});
  }
}

export async function withdrawShortPost(page, payload, network = {}) {
  const expected = content(payload, true);
  const config = settings(network);
  if (!expected || !config)
    return result("unsupported", "invalid_content_payload", "withdraw");
  let attempted = false;
  const evidence = [];
  let publicContext;
  let captureRequest;
  try {
    const account = await owner(page, config);
    if (!account)
      return result("unknown", "account_ownership_unverified", "withdraw");
    const url = `${config.origin}/pin/${expected.external_id}`;
    const response = await page.goto(url, {
      waitUntil: "domcontentloaded",
      timeout: config.timeout,
    });
    if (response?.status() !== 200)
      return result("unknown", "short_post_presence_unverified", "withdraw");
    const card = await item(page, url, expected, account, config);
    if (!card || !sameOwner(account, await owner(page, config)))
      return result("unknown", "account_ownership_unverified", "withdraw");
    if ((await page.locator(MENU).count()) !== 0)
      return result("unsupported", "short_post_menu_ambiguous", "withdraw");
    const confirmation = page.locator(".ConfirmModal:visible").filter({
      has: page.getByText(DELETE_TITLE, { exact: true }),
    });
    if ((await confirmation.count()) !== 0)
      return result("unknown", "short_post_confirmation_ambiguous", "withdraw");
    await card
      .locator(
        '.PinToolbar-actions button[aria-label="设置"][aria-haspopup="true"]',
      )
      .click({ timeout: config.timeout });
    const menu = page.locator(MENU);
    await menu.waitFor({ state: "visible", timeout: config.timeout });
    const remove = menu.getByRole("button", { name: "删除", exact: true });
    if ((await menu.count()) !== 1 || (await remove.count()) !== 1)
      return result(
        "unsupported",
        "short_post_delete_control_unverified",
        "withdraw",
      );
    attempted = true;
    const attempt = {
      kind: "withdrawal_attempt",
      external_id: expected.external_id,
      url,
      placement_slot: "short_post",
      expected_sha256: hash(expected.body),
      observed_at: new Date().toISOString(),
      delete_clicked: true,
      confirmation_clicked: false,
      withdrawal_verified: false,
    };
    evidence.push(attempt);
    await remove.click({ timeout: config.timeout });
    await confirmation
      .first()
      .waitFor({ state: "visible", timeout: config.timeout });
    const confirm = confirmation.getByRole("button", {
      name: "确认",
      exact: true,
    });
    if (
      (await confirmation.count()) !== 1 ||
      (await confirmation.getByText(DELETE_TITLE, { exact: true }).count()) !==
        1 ||
      (await confirm.count()) !== 1
    )
      return result(
        "unknown",
        "short_post_confirmation_ambiguous",
        "withdraw",
        evidence,
      );
    if (!sameOwner(account, await owner(page, config)))
      return result(
        "unknown",
        "account_ownership_unverified",
        "withdraw",
        evidence,
      );

    // Match the request created by this confirmation, not a pre-existing response.
    const deleteUrl = `${config.origin}/api/v4/pins/${expected.external_id}`;
    let deleteRequest;
    let requestCount = 0;
    captureRequest = (request) => {
      if (
        request.method() === "DELETE" &&
        request.url() === deleteUrl &&
        request.frame() === page.mainFrame()
      ) {
        requestCount++;
        deleteRequest ??= request;
      }
    };
    page.on("request", captureRequest);
    const pendingResponse = page
      .waitForResponse((receipt) => receipt.request() === deleteRequest, {
        timeout: config.timeout,
      })
      .catch(() => null);
    attempt.confirmation_clicked = true;
    const [, receipt] = await Promise.all([
      confirm.click({ timeout: config.timeout }),
      pendingResponse,
    ]);
    if (!receipt)
      return result(
        "unknown",
        "short_post_delete_response_unverified",
        "withdraw",
        evidence,
      );
    evidence.push({
      kind: "withdrawal_response",
      external_id: expected.external_id,
      url: deleteUrl,
      method: "DELETE",
      http_status: receipt.status(),
      observed_at: new Date().toISOString(),
    });
    if (requestCount !== 1 || !receipt.ok())
      return result(
        "unknown",
        "short_post_delete_response_unverified",
        "withdraw",
        evidence,
      );
    if (!sameOwner(account, await owner(page, config)))
      return result(
        "unknown",
        "account_ownership_unverified",
        "withdraw",
        evidence,
      );

    // Optimistic removal and the success toast are not deletion receipts.
    // Only a correlated successful DELETE permits an anonymous absence check.
    publicContext = await page
      .context()
      .browser()
      .newContext(config.proxy ? { proxy: config.proxy } : {});
    const publicPage = await publicContext.newPage();
    const absent = await publicPage.goto(url, {
      waitUntil: "domcontentloaded",
      timeout: config.timeout,
    });
    const verified =
      pinUrl(publicPage.url(), config.origin) === url &&
      [404, 410].includes(absent?.status()) &&
      (await publicPage.locator(".PinItem:visible").count()) === 0 &&
      requestCount === 1;
    const observedAt = new Date().toISOString();
    evidence.push({
      kind: "public_absence",
      external_id: expected.external_id,
      url,
      http_status: absent?.status() ?? null,
      absence_verified: verified,
      observed_at: observedAt,
    });
    if (!verified)
      return result(
        "unknown",
        "short_post_absence_unverified",
        "withdraw",
        evidence,
      );
    attempt.withdrawal_verified = true;
    return {
      ...result("completed", null, "withdraw", evidence),
      external_id: expected.external_id,
      public_url: url,
      occurred_at: observedAt,
    };
  } catch {
    return result(
      attempted ? "unknown" : "unsupported",
      attempted
        ? "short_post_withdrawal_outcome_unverified"
        : "short_post_delete_control_unverified",
      "withdraw",
      evidence,
    );
  } finally {
    if (captureRequest) page.off("request", captureRequest);
    await publicContext?.close();
  }
}
