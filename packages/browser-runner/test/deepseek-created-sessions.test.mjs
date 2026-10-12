import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import test from "node:test";
import { observeDeepSeekCreatedSessions } from "../src/deepseek-created-sessions.mjs";

const endpoint = "https://chat.deepseek.com/api/v0/chat_session/create";
const tick = () => new Promise((resolve) => setImmediate(resolve));
const deferred = () => {
  let resolve;
  const promise = new Promise((done) => {
    resolve = done;
  });
  return { promise, resolve };
};
const bytes = (id = "synthetic-first") =>
  Buffer.from(
    JSON.stringify({
      code: 0,
      data: {
        biz_code: 0,
        biz_data: {
          chat_session: {
            id,
            title: "discarded-metadata",
            account: "not-account-proof",
          },
          ttl_seconds: 60,
        },
      },
    }),
  );
class Page extends EventEmitter {
  url = "https://chat.deepseek.com/";
  frame = { url: () => this.url };
  mainFrame() {
    return this.frame;
  }
}
function request(page, overrides = {}) {
  return {
    frame: () => page.frame,
    url: () => endpoint,
    method: () => "POST",
    postData: () => "{}",
    redirectedFrom: () => null,
    isNavigationRequest: () => false,
    ...overrides,
  };
}
function response(req, overrides = {}) {
  return {
    request: () => req,
    url: () => endpoint,
    status: () => 200,
    headerValue: async () => null,
    body: async () => bytes(),
    ...overrides,
  };
}
function emit(page, id, overrides = {}) {
  const req = request(page);
  page.emit("request", req);
  page.emit(
    "response",
    response(req, { body: async () => bytes(id), ...overrides }),
  );
  return req;
}
function clean(page) {
  for (const event of [
    "request",
    "response",
    "requestfailed",
    "framenavigated",
    "close",
  ])
    assert.equal(page.listenerCount(event), 0, event);
}

test("exact prefetched session wins over replenishment; no metadata/account inference", async () => {
  const page = new Page();
  const observer = observeDeepSeekCreatedSessions(page);
  emit(page, "synthetic-first");
  emit(page, "synthetic-replenishment");
  await tick();
  page.url = "https://chat.deepseek.com/a/chat/s/synthetic-first";
  page.emit("framenavigated", page.frame);
  assert.deepEqual(await observer.lookup("synthetic-first"), {
    chatSessionId: "synthetic-first",
    source: "browser_observed_create",
  });
  assert.equal(await observer.lookup("synthetic-unknown"), null);
  observer.dispose();
  clean(page);
});

test("lookup waits for already observed response body, including duplicate response event", async () => {
  const page = new Page();
  const observer = observeDeepSeekCreatedSessions(page);
  const body = deferred();
  const req = emit(page, "unused", { body: () => body.promise });
  await tick();
  let settled = false;
  const found = observer.lookup("synthetic-first").then((value) => {
    settled = true;
    return value;
  });
  page.emit("response", response(req));
  await tick();
  assert.equal(settled, false);
  body.resolve(bytes());
  assert.equal((await found).chatSessionId, "synthetic-first");
  observer.dispose();
});

test("waits for pending duplicate candidate then fails closed rather than overwrite", async () => {
  const page = new Page();
  const observer = observeDeepSeekCreatedSessions(page);
  emit(page, "synthetic-first");
  const body = deferred();
  emit(page, "unused", { body: () => body.promise });
  await tick();
  const found = observer.lookup("synthetic-first");
  body.resolve(bytes());
  assert.equal(await found, null);
  assert.equal(observer.status(), "ambiguous");
  clean(page);
});

test("ignores pre-arm, child-frame, redirects, nonempty body and nonexact endpoints", async () => {
  const page = new Page();
  const old = request(page);
  page.emit("request", old);
  const observer = observeDeepSeekCreatedSessions(page);
  let reads = 0;
  const body = async () => {
    reads++;
    return bytes();
  };
  page.emit("response", response(old, { body }));
  for (const override of [
    { frame: () => ({}) },
    { redirectedFrom: () => ({}) },
    { method: () => "GET" },
    { postData: () => '{"title":"discard"}' },
    { postData: () => "[]" },
    { postData: () => "null" },
    { postData: () => "broken" },
    { url: () => endpoint + "?extra=1" },
    { url: () => endpoint + "/" },
    { url: () => "https://example.invalid/api/v0/chat_session/create" },
  ]) {
    const req = request(page, override);
    page.emit("request", req);
    page.emit("response", response(req, { body }));
  }
  await tick();
  assert.equal(reads, 0);
  assert.equal(await observer.lookup("synthetic-first"), null);
  observer.dispose();
});

test("initial document navigation allowed, replacement invalidates old delayed body", async () => {
  const page = new Page();
  const observer = observeDeepSeekCreatedSessions(page);
  const navigate = () => {
    page.emit("request", request(page, { isNavigationRequest: () => true }));
    page.emit("framenavigated", page.frame);
  };
  navigate();
  const body = deferred();
  emit(page, "unused", { body: () => body.promise });
  await tick();
  const old = observer.lookup("synthetic-first");
  navigate();
  assert.equal(await old, null);
  emit(page, "synthetic-new");
  body.resolve(bytes());
  await tick();
  assert.equal(await observer.lookup("synthetic-first"), null);
  assert.equal(
    (await observer.lookup("synthetic-new")).chatSessionId,
    "synthetic-new",
  );
  observer.dispose();
});

test("overflow explicitly unavailable, never falls back to latest", async () => {
  const page = new Page();
  const observer = observeDeepSeekCreatedSessions(page, { maxCandidates: 1 });
  emit(page, "synthetic-first");
  await tick();
  emit(page, "synthetic-second");
  assert.equal(observer.status(), "overflow");
  assert.equal(await observer.lookup("synthetic-first"), null);
  clean(page);
});

test("malformed/failed/oversized responses never overwrite valid evidence", async () => {
  const page = new Page();
  const observer = observeDeepSeekCreatedSessions(page);
  emit(page, "synthetic-first");
  for (const override of [
    { body: async () => Buffer.from("{") },
    { body: async () => Buffer.from([0xff]) },
    { body: async () => bytes("invalid id") },
    { body: async () => Buffer.from('{"code":1,"data":{"biz_code":0}}') },
    { body: async () => Buffer.alloc(65537) },
    { status: () => 302 },
    { headerValue: async () => "999999" },
    {
      body: async () => {
        throw Error("discarded-private-detail");
      },
    },
  ])
    emit(page, "unused", override);
  await tick();
  assert.equal(
    (await observer.lookup("synthetic-first")).chatSessionId,
    "synthetic-first",
  );
  assert.equal(observer.status(), "observing");
  observer.dispose();
});

test("dispose, cancellation, close and timeout resolve waiters and late reads remain inert", async () => {
  for (const action of ["dispose", "cancelled", "closed", "timeout"]) {
    const page = new Page();
    const controller = new AbortController();
    const observer = observeDeepSeekCreatedSessions(page, {
      signal: controller.signal,
      timeoutMs: action === "timeout" ? 20 : 1000,
    });
    const body = deferred();
    emit(page, "unused", { body: () => body.promise });
    await tick();
    const found = observer.lookup("synthetic-first");
    if (action === "dispose") observer.dispose();
    if (action === "cancelled") controller.abort();
    if (action === "closed") page.emit("close");
    assert.equal(await found, null);
    body.resolve(bytes());
    await tick();
    assert.equal(await observer.lookup("synthetic-first"), null);
    clean(page);
  }
});

test("failed request releases pending lookup; invalid options attach no listeners", async () => {
  const page = new Page();
  const observer = observeDeepSeekCreatedSessions(page);
  const req = request(page);
  page.emit("request", req);
  const result = observer.lookup("synthetic-first");
  page.emit("requestfailed", req);
  assert.equal(await result, null);
  observer.dispose();
  for (const options of [
    { maxCandidates: 17 },
    { maxBytes: Infinity },
    { timeoutMs: 0 },
  ]) {
    const invalid = observeDeepSeekCreatedSessions(page, options);
    assert.equal(invalid.status(), "invalid_options");
    clean(page);
  }
});

test("synchronous proof validity rejects copies, pending creates, navigation and stopped observers", async () => {
  const page = new Page();
  const observer = observeDeepSeekCreatedSessions(page);
  try {
    emit(page, "synthetic-first");
    const proof = await observer.lookup("synthetic-first");
    assert.equal(observer.isCurrent(proof), true);
    assert.equal(observer.isCurrent({ ...proof }), false);
    assert.equal(observer.isCurrent(null), false);
    assert.equal(observer.isCurrent(undefined), false);
    const pending = request(page);
    page.emit("request", pending);
    assert.equal(observer.isCurrent(proof), false);
    page.emit("requestfailed", pending);
    assert.equal(observer.isCurrent(proof), true);
    page.url = "https://deepseek.example.invalid/";
    assert.equal(observer.isCurrent(proof), false);
    page.url = "https://chat.deepseek.com/";
    page.emit("request", request(page, { isNavigationRequest: () => true }));
    assert.equal(observer.status(), "observing");
    assert.equal(observer.isCurrent(proof), false);
    page.emit("framenavigated", page.frame);
    emit(page, "synthetic-first");
    const replacement = await observer.lookup("synthetic-first");
    assert.equal(observer.isCurrent(proof), false);
    assert.equal(observer.isCurrent(replacement), true);
    observer.dispose();
    assert.equal(observer.isCurrent(replacement), false);
  } finally {
    observer.dispose();
  }
});

test("resolved lookup proof is not current after invalidation before its await continuation", async () => {
  const page = new Page();
  const observer = observeDeepSeekCreatedSessions(page);
  try {
    emit(page, "synthetic-first");
    const proof = await observer.lookup("synthetic-first");
    const unrelated = request(page);
    page.emit("request", unrelated);
    const lookup = observer.lookup("synthetic-first");
    // A pending create completes and resolves waiters. Navigation then
    // invalidates the proof before any promise continuation gets to use it.
    page.emit("requestfailed", unrelated);
    page.emit("request", request(page, { isNavigationRequest: () => true }));
    assert.equal(await lookup, proof);
    assert.equal(observer.status(), "observing");
    assert.equal(observer.isCurrent(proof), false);
  } finally {
    observer.dispose();
  }
});
