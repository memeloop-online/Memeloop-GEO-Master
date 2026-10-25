import {
  CONNECTOR_VERSION,
  contentPayload,
  defineProvider,
  ownIdentity,
  probeOwnAccount,
  unknown,
  unsupported,
} from "./contract.mjs";

const BAIDU_SELF = "https://baijiahao.baidu.com/builder/app/appinfo";
const BAIDU_EDITOR = "https://baijiahao.baidu.com/builder/rc/edit?type=news";

export function baiduIdentity(data) {
  const user = data?.data?.user;
  return ownIdentity(user?.userid, user?.name, user?.avatar);
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

export const baiduCreatorAdapter = defineProvider({
  loginEntryAvailable: true,
  loginSupported: true,
  connectorVersion: CONNECTOR_VERSION,
  origin: "https://baijiahao.baidu.com",
  entry: "https://baijiahao.baidu.com/builder/theme/bjh/login",
  operations: ["publish", "lookup"],
  identify(page) {
    return probeOwnAccount(page, BAIDU_SELF, baiduIdentity);
  },
  async execute(page, operation, payload) {
    if (operation === "publish") return publishBaidu(page, payload);
    return unsupported("public_lookup_unverified");
  },
});
