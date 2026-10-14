import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import test from "node:test";
import { chromium } from "playwright";
import {
  parseDoubaoMenuConfiguration,
  parseDoubaoActionBarConfiguration,
  observeDoubaoMenuConfiguration,
} from "../src/doubao-web-configuration.mjs";

const bytes = (value) => Buffer.from(JSON.stringify(value));
const model = () => ({
  item_id: 101,
  model_item_key: "selector-a",
  name: "Synthetic model",
  need_login: true,
  subscribe_config: {
    need_upgrade: true,
    jump_url: "https://example.invalid/subscription",
  },
});
const menu = () => ({
  use_mode_model_list: true,
  mode_list: {
    item_list: [
      {
        mode_id: "mode-a",
        name: "Synthetic mode",
        need_login: false,
        default_model: { item_id: "101", model_item_key: "selector-a" },
        model_list: { item_list: [model()] },
      },
    ],
  },
  model_list: { item_list: [] },
});
const entry = () => ({
  action_bar_key: "coco_deep_thinking",
  active_switch_conf: { use_menu_conf_v2: true, menu_conf_v2: menu() },
});
const envelope = () => ({ code: 0, data: { entry_list: [entry()] } });
const endpoint = "https://www.doubao.com/alice/slot/action_bar_v3/brief_list";
const expectedModel = () => ({
  selector: "selector-a",
  model_item_key: "selector-a",
  item_id: 101,
  name: "Synthetic model",
  need_login: true,
  need_upgrade: true,
});

test("candidate projection preserves scoped selectors and strips defaults and opaque fields", () => {
  assert.deepEqual(parseDoubaoMenuConfiguration(bytes(menu())), {
    use_mode_model_list: true,
    mode_configs: [
      {
        mode_id: "mode-a",
        name: "Synthetic mode",
        need_login: false,
        model_configs: [expectedModel()],
      },
    ],
    model_configs: [],
  });
  const input = menu();
  input.mode_list.item_list.push({
    ...input.mode_list.item_list[0],
    mode_id: "mode-b",
  });
  input.model_list.item_list = [model()];
  const result = parseDoubaoMenuConfiguration(bytes(input));
  assert.equal(result.mode_configs.length, 2);
  assert.deepEqual(result.model_configs, [expectedModel()]);
});

test("unknown gates stay unknown and numeric IDs are only fallback UI selectors", () => {
  const input = menu();
  input.use_mode_model_list = false;
  input.mode_list.item_list = [];
  input.model_list.item_list = [
    { item_id: 102, name: "Synthetic", need_login: "false" },
    { item_id: "103", name: "Synthetic other", subscribe_config: {} },
  ];
  assert.deepEqual(
    parseDoubaoMenuConfiguration(bytes(input)).model_configs.map((item) => [
      item.selector,
      item.need_login,
      item.need_upgrade,
    ]),
    [
      ["102", null, null],
      ["103", null, null],
    ],
  );
});

test("rejects malformed menus, duplicate scopes/selectors and byte bounds", () => {
  const invalid = [null, {}, { ...menu(), use_mode_model_list: 1 }];
  for (const change of [
    (input) => input.mode_list.item_list.push(input.mode_list.item_list[0]),
    (input) => delete input.mode_list.item_list[0].model_list,
    (input) => (input.model_list = null),
    (input) => (input.mode_list.item_list[0].mode_id = "bad id"),
    (input) => (input.mode_list.item_list[0].name = "\n"),
    (input) => input.mode_list.item_list[0].model_list.item_list.push(model()),
    (input) =>
      (input.mode_list.item_list[0].model_list.item_list[0].model_item_key =
        ""),
    (input) =>
      (input.mode_list.item_list[0].model_list.item_list[0].item_id = 1.5),
    (input) => (input.mode_list.item_list[0].model_list.item_list = []),
    (input) => (input.mode_list.item_list = Array(65).fill({})),
  ]) {
    const input = menu();
    change(input);
    invalid.push(input);
  }
  for (const input of invalid)
    assert.equal(parseDoubaoMenuConfiguration(bytes(input)), null);
  for (const input of [Buffer.from([0xff]), Buffer.from("{"), "not bytes"])
    assert.equal(parseDoubaoMenuConfiguration(input), null);
  for (const maxBytes of [0, 1, Infinity, 128 * 1024 + 1])
    assert.equal(
      parseDoubaoMenuConfiguration(bytes(menu()), { maxBytes }),
      null,
    );
});

test("raw envelope follows verified numeric success precedence and exact action key", () => {
  assert.ok(parseDoubaoActionBarConfiguration(bytes(envelope())));
  for (const code of [undefined, null]) {
    assert.ok(
      parseDoubaoActionBarConfiguration(
        bytes({ ...envelope(), code, status_code: 0 }),
      ),
    );
  }
  for (const value of [
    menu(),
    { entry_list: [entry()] },
    { ...envelope(), code: undefined },
    { ...envelope(), code: "0" },
    { ...envelope(), code: 1, status_code: 0 },
    { ...envelope(), data: { entry_list: [entry(), entry()] } },
    {
      ...envelope(),
      data: { entry_list: [{ ...entry(), action_bar_key: "other" }] },
    },
    {
      ...envelope(),
      data: {
        entry_list: [
          {
            ...entry(),
            active_switch_conf: {
              use_menu_conf_v2: false,
              menu_conf_v2: menu(),
            },
          },
        ],
      },
    },
  ])
    assert.equal(parseDoubaoActionBarConfiguration(bytes(value)), null);
});

class Page extends EventEmitter {
  frame = { url: () => "https://www.doubao.com/chat/" };
  mainFrame() {
    return this.frame;
  }
}
function response(page, overrides = {}) {
  const request = {
    method: () => "POST",
    frame: () => page.frame,
    isNavigationRequest: () => false,
  };
  return {
    url: () => endpoint,
    request: () => request,
    status: () => 200,
    headerValue: async () => null,
    body: async () => bytes(envelope()),
    ...overrides,
  };
}
const emitResponse = (page, value) => {
  page.emit("request", value.request());
  page.emit("response", value);
};
const clean = (page) => {
  for (const event of ["request", "response", "close", "framenavigated"])
    assert.equal(page.listenerCount(event), 0);
};
const tick = () => new Promise((resolve) => setImmediate(resolve));
const navigate = (page) => {
  page.emit("request", {
    frame: () => page.frame,
    isNavigationRequest: () => true,
  });
  page.emit("framenavigated", page.frame);
};

test("observer ignores wrong origin/path/method, subframes and pre-arm requests", async () => {
  const page = new Page();
  const early = response(page, { body: () => assert.fail("pre-arm") });
  page.emit("request", early.request());
  const observer = observeDoubaoMenuConfiguration(page);
  page.emit("response", early);
  for (const url of [
    "https://example.invalid/alice/slot/action_bar_v3/brief_list",
    `${endpoint}/extra`,
    `${endpoint}#fragment`,
    endpoint.replace("www.", ""),
  ])
    emitResponse(
      page,
      response(page, { url: () => url, body: () => assert.fail("wrong URL") }),
    );
  for (const request of [
    {
      method: () => "GET",
      frame: () => page.frame,
      isNavigationRequest: () => false,
    },
    {
      method: () => "POST",
      frame: () => ({}),
      isNavigationRequest: () => false,
    },
  ])
    emitResponse(
      page,
      response(page, {
        request: () => request,
        body: () => assert.fail("wrong request"),
      }),
    );
  emitResponse(page, response(page));
  assert.ok(await observer.result);
  clean(page);
});

test("first matching invalid response wins and rejected bodies never escape", async () => {
  for (const overrides of [
    { status: () => 403 },
    {
      headerValue: async () => "999999",
      body: () => assert.fail("oversized header"),
    },
    { headerValue: async () => "invalid" },
    { body: async () => Buffer.alloc(128 * 1024 + 1) },
    { body: async () => Buffer.from("{") },
    {
      body: async () => {
        throw new Error("synthetic");
      },
    },
  ]) {
    const page = new Page();
    const observer = observeDoubaoMenuConfiguration(page);
    emitResponse(page, response(page, overrides));
    emitResponse(
      page,
      response(page, { body: () => assert.fail("second response") }),
    );
    assert.equal(await observer.result, null);
    clean(page);
  }
});

test("abort/close/dispose/deadline and invalid options remove all listeners", async () => {
  for (const action of [
    "abort",
    "close",
    "dispose",
    "timeout",
    "pre-abort",
    "invalid",
  ]) {
    const page = new Page();
    const controller = new AbortController();
    if (action === "pre-abort") controller.abort();
    const observer = observeDoubaoMenuConfiguration(page, {
      signal: controller.signal,
      timeoutMs: action === "invalid" ? -1 : 5,
    });
    if (action === "abort") controller.abort();
    if (action === "close") page.emit("close");
    if (action === "dispose") observer.dispose();
    assert.equal(await observer.result, null);
    clean(page);
  }
});

test("navigation invalidates old requests and pending headers or bodies", async () => {
  for (const stage of ["unclaimed", "header", "body"]) {
    const page = new Page();
    const observer = observeDoubaoMenuConfiguration(page);
    let complete;
    const pending = new Promise((resolve) => {
      complete = resolve;
    });
    const old = response(
      page,
      stage === "header"
        ? {
            headerValue: () => pending,
            body: () => assert.fail("stale header"),
          }
        : { body: () => pending },
    );
    page.emit("request", old.request());
    if (stage !== "unclaimed") page.emit("response", old);
    await tick();
    navigate(page);
    if (stage === "unclaimed") {
      page.emit("response", old);
      emitResponse(page, response(page));
      assert.ok(await observer.result);
    } else {
      assert.equal(await observer.result, null);
    }
    complete(stage === "header" ? null : bytes(envelope()));
    await tick();
    clean(page);
  }
});

test("disposal makes late header/body resolution and rejection inert", async () => {
  for (const stage of ["header", "body", "reject"]) {
    const page = new Page();
    let complete;
    const pending = new Promise((resolve, reject) => {
      complete = stage === "reject" ? reject : resolve;
    });
    const observer = observeDoubaoMenuConfiguration(page);
    emitResponse(
      page,
      response(
        page,
        stage === "header"
          ? {
              headerValue: () => pending,
              body: () => assert.fail("disposed header"),
            }
          : { body: () => pending },
      ),
    );
    await tick();
    observer.dispose();
    complete(
      stage === "header"
        ? null
        : stage === "reject"
          ? new Error("synthetic")
          : bytes(envelope()),
    );
    await tick();
    assert.equal(await observer.result, null);
    clean(page);
  }
});

test("real browser observes only a site-generated POST after caller navigation", async () => {
  const browser = await chromium.launch({
    headless: true,
    ...(process.env.GEO_TEST_CHROMIUM_PATH
      ? { executablePath: process.env.GEO_TEST_CHROMIUM_PATH }
      : {}),
  });
  try {
    const context = await browser.newContext({ serviceWorkers: "block" });
    const page = await context.newPage();
    let posts = 0;
    await context.route("**/*", async (route) => {
      const request = route.request();
      if (request.url() === endpoint && request.method() === "POST") {
        posts++;
        return route.fulfill({
          contentType: "application/json",
          body: JSON.stringify(envelope()),
        });
      }
      if (
        request.isNavigationRequest() &&
        request.url() === "https://www.doubao.com/chat/"
      )
        return route.fulfill({
          contentType: "text/html",
          body: `<script>fetch("/alice/slot/action_bar_v3/brief_list",{method:"POST"})</script>`,
        });
      return route.abort();
    });
    const events = ["request", "response", "close", "framenavigated"];
    const baseline = events.map((event) => page.listenerCount(event));
    const observer = observeDoubaoMenuConfiguration(page);
    await page.goto("https://www.doubao.com/chat/");
    assert.ok(await observer.result);
    assert.equal(posts, 1);
    assert.deepEqual(
      events.map((event) => page.listenerCount(event)),
      baseline,
    );
  } finally {
    await browser.close();
  }
});
