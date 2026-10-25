import {
  CONNECTOR_VERSION,
  defineProvider,
  ownIdentity,
  unsupported,
} from "./contract.mjs";

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

export const xiaohongshuAdapter = defineProvider({
  loginEntryAvailable: true,
  loginSupported: true,
  connectorVersion: CONNECTOR_VERSION,
  origin: "https://creator.xiaohongshu.com",
  entry: "https://creator.xiaohongshu.com/login",
  operations: ["publish", "lookup"],
  identify(page) {
    return xiaohongshuIdentity(page);
  },
  async execute() {
    return unsupported("media_or_article_flow_unverified");
  },
});
