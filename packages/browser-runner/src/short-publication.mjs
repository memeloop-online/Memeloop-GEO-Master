import { createHash } from "node:crypto";
import { probeOwnAccount, zhihuIdentity } from "./adapters.mjs";

export const SHORT_PUBLICATION_VERSION = "zhihu.short_post.ui.v1";
const ORIGIN = "https://www.zhihu.com";
const FORM = ".WritePinV2-Form";
const EDITOR = '.public-DraftEditor-content[contenteditable="true"]';
const MENU = ".Popover-content.PinToolbar-menuContainer:visible";

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
    // There is exactly one click. Any exception after this point is unknown.
    submitted = true;
    await submit.click({ timeout: config.timeout });
    await page.waitForURL((url) => pinUrl(url.href, config.origin) !== null, {
      timeout: config.timeout,
    });
    const candidate = {
      url: pinUrl(page.url(), config.origin),
      source: "post_submit_navigation",
    };
    const externalId = new URL(candidate.url).pathname.slice("/pin/".length);
    const observedAt = new Date().toISOString();
    const evidence = {
      kind: "publication_candidate",
      schema_version: "geo.publication.candidate.v1",
      url: candidate.url,
      expected_sha256: hash(expected.body),
      observed_at: observedAt,
      source: candidate.source,
    };
    const outcome = await readback(
      page,
      { ...expected, external_id: externalId },
      config,
      account,
    );
    return {
      ...outcome,
      external_id: externalId,
      public_url: candidate.url,
      evidence: [evidence, ...outcome.evidence],
    };
  } catch {
    return result(
      submitted ? "unknown" : "unsupported",
      submitted
        ? "submission_outcome_unverified"
        : "editor_selectors_unverified",
      stage,
    );
  }
}

export async function withdrawShortPost(page, payload, network = {}) {
  const expected = content(payload, true);
  const config = settings(network);
  if (!expected || !config)
    return result("unsupported", "invalid_content_payload", "withdraw");
  let attempted = false;
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
    await remove.click({ timeout: config.timeout });
    // The known item-local command may itself delete or open a confirmation.
    // Its confirmation and authoritative deletion receipt are not yet proven.
    // Never guess a second destructive control or claim success from absence.
    return result(
      "unknown",
      "short_post_withdrawal_confirmation_unverified",
      "withdraw",
      [
        {
          kind: "withdrawal_attempt",
          external_id: expected.external_id,
          url,
          placement_slot: "short_post",
          expected_sha256: hash(expected.body),
          observed_at: new Date().toISOString(),
          delete_clicked: true,
          withdrawal_verified: false,
        },
      ],
    );
  } catch {
    return result(
      attempted ? "unknown" : "unsupported",
      attempted
        ? "short_post_withdrawal_outcome_unverified"
        : "short_post_delete_control_unverified",
      "withdraw",
    );
  }
}
