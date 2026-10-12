import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import test from "node:test";
import {
  observeDeepSeekModelConfiguration,
  parseDeepSeekModelConfiguration,
} from "../src/deepseek-web-configuration.mjs";

const model = () => ({
  model_type: "synthetic-model",
  enabled: true,
  switchable: true,
  is_default: true,
  search_feature: false,
  think_feature: true,
});
const envelope = (models = [model()]) => ({
  code: 0,
  data: {
    biz_code: 0,
    biz_data: {
      version: 1,
      settings: { model_configs: { id: 1, value: models } },
    },
  },
});
const bytes = (value) => Buffer.from(JSON.stringify(value));
const mainFrame = { url: () => "https://chat.deepseek.com/" };
class Page extends EventEmitter {
  mainFrame() {
    return mainFrame;
  }
  emit(name, value) {
    if (name === "response") super.emit("request", value.request());
    return super.emit(name, value);
  }
}
const response = (overrides = {}) => {
  const request = {
    method: () => "GET",
    frame: () => mainFrame,
    isNavigationRequest: () => false,
  };
  return {
    url: () =>
      "https://chat.deepseek.com/api/v0/client/settings?did=synthetic&scope=model",
    request: () => request,
    status: () => 200,
    headerValue: async () => null,
    body: async () => bytes(envelope()),
    ...overrides,
  };
};
const tick = () => new Promise((resolve) => setImmediate(resolve));
const clean = (page) => {
  assert.equal(page.listenerCount("response"), 0);
  assert.equal(page.listenerCount("close"), 0);
  assert.equal(page.listenerCount("request"), 0);
  assert.equal(page.listenerCount("framenavigated"), 0);
};

test("normalizes only verified configuration, never guesses selection", () => {
  const input = envelope([{ ...model(), secret: "discard" }]);
  assert.deepEqual(parseDeepSeekModelConfiguration(bytes(input)), {
    model_configs: [model()],
    selected_model: null,
  });
});

test("rejects malformed envelopes, flags, identifiers and bounds", () => {
  const invalid = [
    null,
    {},
    { ...envelope(), code: 1 },
    { data: envelope().data },
    { ...envelope(), data: { biz_code: 1 } },
    envelope([{ ...model(), model_type: "bad id" }]),
    envelope([{ ...model(), enabled: 1 }]),
    envelope([{ ...model(), switchable: false }]),
    envelope([model(), model()]),
    envelope([]),
    envelope(
      Array.from({ length: 65 }, (_, i) => ({
        ...model(),
        model_type: `model-${i}`,
      })),
    ),
  ];
  for (const value of invalid)
    assert.equal(parseDeepSeekModelConfiguration(bytes(value)), null);
  const missing = envelope();
  delete missing.data.biz_data.version;
  assert.equal(parseDeepSeekModelConfiguration(bytes(missing)), null);
  assert.equal(parseDeepSeekModelConfiguration(Buffer.from([0xff])), null);
  assert.equal(parseDeepSeekModelConfiguration(Buffer.from("{")), null);
  assert.equal(
    parseDeepSeekModelConfiguration(bytes(envelope()), { maxBytes: 2 }),
    null,
  );
  assert.equal(
    parseDeepSeekModelConfiguration(bytes(envelope()), { maxBytes: Infinity }),
    null,
  );
});

test("unknown optional feature shapes are omitted, never coerced to capabilities", () => {
  const result = parseDeepSeekModelConfiguration(
    bytes(
      envelope([
        {
          ...model(),
          think_feature: {},
          search_feature: "yes",
          file_feature: { conflict_with_search: true },
        },
      ]),
    ),
  );
  assert.deepEqual(result, {
    model_configs: [
      {
        model_type: "synthetic-model",
        enabled: true,
        switchable: true,
        is_default: true,
      },
    ],
    selected_model: null,
  });
});

test("ignores wrong origins, paths, scopes and methods without reading bodies", async () => {
  const page = new Page();
  const observer = observeDeepSeekModelConfiguration(page);
  let reads = 0;
  const body = async () => {
    reads++;
    return bytes(envelope());
  };
  for (const url of [
    "https://example.invalid/api/v0/client/settings?scope=model",
    "https://chat.deepseek.com/api/v0/client/settings/other?scope=model",
    "https://chat.deepseek.com/api/v0/client/settings?scope=other",
    "https://chat.deepseek.com/api/v0/client/settings?scope=model&scope=model",
  ])
    page.emit("response", response({ url: () => url, body }));
  const wrongMethod = {
    method: () => "POST",
    frame: () => mainFrame,
    isNavigationRequest: () => false,
  };
  page.emit("response", response({ request: () => wrongMethod, body }));
  page.emit("response", response({ body }));
  assert.ok(await observer.result);
  assert.equal(reads, 1);
  clean(page);
});

test("rejects bad status, declared/actual oversized bodies and malformed JSON", async () => {
  for (const overrides of [
    { status: () => 403 },
    {
      headerValue: async () => "999999999",
      body: () => assert.fail("must not read"),
    },
    { headerValue: async () => "invalid" },
    { body: async () => Buffer.alloc(128 * 1024 + 1) },
    { body: async () => Buffer.from("{") },
    {
      body: async () => {
        throw new Error("private error");
      },
    },
  ]) {
    const page = new Page();
    const observer = observeDeepSeekModelConfiguration(page);
    page.emit("response", response(overrides));
    assert.equal(await observer.result, null);
    clean(page);
  }
});

test("abort, close, dispose and timeout settle once and remove listeners", async () => {
  for (const action of [
    "abort",
    "close",
    "dispose",
    "timeout",
    "already-aborted",
  ]) {
    const page = new Page();
    const controller = new AbortController();
    if (action === "already-aborted") controller.abort();
    const observer = observeDeepSeekModelConfiguration(page, {
      signal: controller.signal,
      timeoutMs: 5,
    });
    if (action === "abort") controller.abort();
    if (action === "close") page.emit("close");
    if (action === "dispose") observer.dispose();
    assert.equal(await observer.result, null);
    observer.dispose();
    clean(page);
  }
});

test("late body resolution/rejection is inert and second response cannot replace first", async () => {
  for (const reject of [false, true]) {
    const page = new Page();
    let complete;
    const pending = new Promise((resolve, rejectPromise) => {
      complete = reject ? rejectPromise : resolve;
    });
    const observer = observeDeepSeekModelConfiguration(page);
    page.emit("response", response({ body: () => pending }));
    await tick();
    page.emit(
      "response",
      response({ body: () => assert.fail("second body read") }),
    );
    observer.dispose();
    assert.equal(await observer.result, null);
    complete(reject ? new Error("private") : bytes(envelope()));
    await tick();
    assert.equal(await observer.result, null);
    clean(page);
  }
});

test("late header completion cannot start a body read after disposal", async () => {
  const page = new Page();
  let complete;
  const pending = new Promise((resolve) => {
    complete = resolve;
  });
  const observer = observeDeepSeekModelConfiguration(page);
  page.emit(
    "response",
    response({
      headerValue: () => pending,
      body: () => assert.fail("body read after disposal"),
    }),
  );
  observer.dispose();
  complete(null);
  await tick();
  assert.equal(await observer.result, null);
  clean(page);
});

const rawResponse = (page, value) =>
  EventEmitter.prototype.emit.call(page, "response", value);
const navigate = (page) => {
  page.emit("request", {
    frame: () => mainFrame,
    isNavigationRequest: () => true,
  });
  page.emit("framenavigated", mainFrame);
};

test("child-frame and pre-arm requests never claim the observation", async () => {
  const page = new Page();
  const early = response({ body: () => assert.fail("pre-arm body") });
  page.emit("request", early.request());
  const observer = observeDeepSeekModelConfiguration(page);
  rawResponse(page, early);
  const childRequest = {
    frame: () => ({ url: mainFrame.url }),
    method: () => "GET",
    isNavigationRequest: () => false,
  };
  page.emit(
    "response",
    response({
      request: () => childRequest,
      body: () => assert.fail("child body"),
    }),
  );
  page.emit("response", response());
  assert.ok(await observer.result);
  clean(page);
});

test("old epoch requests are ignored and caller-owned reload can supply current config", async () => {
  const page = new Page();
  const observer = observeDeepSeekModelConfiguration(page);
  const stale = response({ body: () => assert.fail("previous document body") });
  page.emit("request", stale.request());
  navigate(page);
  rawResponse(page, stale);
  page.emit("response", response());
  assert.ok(await observer.result);
  clean(page);
});

test("main-frame navigation invalidates pending headers and bodies", async () => {
  for (const stage of ["header", "body"]) {
    const page = new Page();
    const observer = observeDeepSeekModelConfiguration(page);
    let complete;
    const pending = new Promise((resolve) => {
      complete = resolve;
    });
    page.emit(
      "response",
      response(
        stage === "header"
          ? {
              headerValue: () => pending,
              body: () => assert.fail("stale header must not start body"),
            }
          : { body: () => pending },
      ),
    );
    await tick();
    navigate(page);
    assert.equal(await observer.result, null);
    complete(stage === "header" ? null : bytes(envelope()));
    await tick();
    assert.equal(await observer.result, null);
    clean(page);
  }
});

test("arming at blank permits intended main-frame navigation, not cross-origin config", async () => {
  const frame = { url: () => "about:blank" };
  const page = new Page();
  page.mainFrame = () => frame;
  const observer = observeDeepSeekModelConfiguration(page);
  const request = {
    frame: () => frame,
    method: () => "GET",
    isNavigationRequest: () => false,
  };
  page.emit(
    "response",
    response({
      request: () => request,
      body: () => assert.fail("blank document config"),
    }),
  );
  frame.url = () => "https://chat.deepseek.com/";
  page.emit("framenavigated", frame);
  page.emit("response", response({ request: () => request }));
  assert.ok(await observer.result);
  clean(page);
});
