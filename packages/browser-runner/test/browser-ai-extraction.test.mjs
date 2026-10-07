import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import test from "node:test";
import { extractWithSignedInBrowser } from "../src/browser-ai-extraction.mjs";

const origin = "https://example.invalid";
const prompt =
  'Extract the synthetic observation. Return {"decision":"unverified"}.';
const frame = (flag, data) => {
  const content = Buffer.from(JSON.stringify(data));
  const header = Buffer.alloc(5);
  header[0] = flag;
  header.writeUInt32BE(content.length, 1);
  return Buffer.concat([header, content]);
};

function fixture({
  output = '{"decision":"unverified"}',
  requestPrompt = prompt,
  complete = true,
  oldAnswer = false,
  markdown = false,
  codeBlock = false,
} = {}) {
  const child = new EventEmitter();
  let sent = 0;
  let closed = 0;
  let created = 0;
  let filled;
  child.goto = async (url) => assert.equal(url, `${origin}/`);
  child.url = () => `${origin}/`;
  child.close = async () => {
    closed += 1;
  };
  child.locator = (selector) => {
    if (selector.includes("assistant")) {
      const content = {
        count: async () => Number(markdown),
        innerText: async () => (codeBlock ? "JSON Copy " + output : output),
        locator: (selector) => {
          assert.equal(selector, "pre code");
          return {
            count: async () => Number(codeBlock),
            innerText: async () => output,
          };
        },
        last: () => content,
      };
      const answer = {
        count: async () => Number(oldAnswer || (sent > 0 && output !== null)),
        innerText: async () =>
          markdown ? "reasoning and action labels" : output,
        locator: (selector) =>
          selector === "pre code" ? content.locator(selector) : content,
      };
      return answer;
    }
    if (selector.includes("chat-input-editor"))
      return {
        isVisible: async () => true,
        fill: async (value) => {
          filled = value;
        },
      };
    assert.equal(selector, ".send-button-container");
    return {
      getAttribute: async () => "send-button-container",
      click: async () => {
        sent += 1;
        child.emit("response", {
          url: () => `${origin}/apiv2/kimi.gateway.chat.v1.ChatService/Chat`,
          request: () => ({
            method: () => "POST",
            headers: () => ({ "content-type": "application/connect+json" }),
            postDataBuffer: () =>
              frame(0, { arbitrary: { scalar: requestPrompt } }),
          }),
          status: () => 200,
          headers: () => ({ "content-type": "application/connect+json" }),
          body: async () =>
            Buffer.concat([
              frame(0, { opaque: "not interpreted by the adapter" }),
              ...(complete ? [frame(2, {})] : []),
            ]),
        });
      },
    };
  };
  const page = {
    context: () => ({
      newPage: async () => {
        created += 1;
        return child;
      },
    }),
    close: () => assert.fail("never close measurement page"),
    goto: () => assert.fail("never navigate measurement page"),
  };
  return {
    page,
    child,
    stats: () => ({ sent, closed, created, filled }),
  };
}
const options = (extra = {}) => ({
  model: "fixture-model",
  trustedOrigin: origin,
  deadlineAt: performance.now() + 200,
  configureModel: async () => true,
  ...extra,
});

test("isolated signed-in conversation submits once and reads only assistant JSON", async () => {
  const f = fixture();
  const result = await extractWithSignedInBrowser(f.page, prompt, options());
  assert.deepEqual(result, {
    extracted: { decision: "unverified" },
    model: "fixture-model",
    surface: "signed_in_browser",
  });
  assert.deepEqual(f.stats(), {
    sent: 1,
    closed: 1,
    created: 1,
    filled: prompt,
  });
  assert.equal(f.child.listenerCount("response"), 0);
});

test("official final markdown excludes reasoning, action labels and code controls", async () => {
  for (const codeBlock of [false, true]) {
    const f = fixture({ markdown: true, codeBlock });
    const result = await extractWithSignedInBrowser(f.page, prompt, options());
    assert.deepEqual(result?.extracted, { decision: "unverified" });
  }
});

test("requires matching prompt and complete stream before reading any JSON", async () => {
  for (const change of [
    { requestPrompt: "unrelated request" },
    { complete: false },
    { output: null },
    { oldAnswer: true },
  ]) {
    const f = fixture(change);
    assert.equal(
      await extractWithSignedInBrowser(
        f.page,
        prompt,
        options({ deadlineAt: performance.now() + 25 }),
      ),
      null,
    );
    assert.equal(f.stats().sent, change.oldAnswer ? 0 : 1);
    assert.equal(f.stats().closed, 1);
  }
});

test("model unavailable, cancelled and expired calls never submit", async () => {
  const cancelled = new AbortController();
  cancelled.abort();
  for (const change of [
    { configureModel: async () => false },
    { signal: cancelled.signal },
    { deadlineAt: performance.now() - 1 },
  ]) {
    const f = fixture();
    assert.equal(
      await extractWithSignedInBrowser(f.page, prompt, options(change)),
      null,
    );
    assert.equal(f.stats().sent, 0);
    assert.equal(f.stats().closed, f.stats().created);
  }
});

test("cancellation during model setup closes own page and prevents late send", async () => {
  const f = fixture();
  const controller = new AbortController();
  let finish;
  const result = await extractWithSignedInBrowser(
    f.page,
    prompt,
    options({
      signal: controller.signal,
      configureModel: () =>
        new Promise((resolve) => {
          finish = resolve;
          controller.abort();
        }),
    }),
  );
  assert.equal(result, null);
  finish(true);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(f.stats().closed, 1);
  assert.equal(f.stats().sent, 0);
});

test("late page creation is closed after deadline without navigation", async () => {
  let finish;
  let closed = 0;
  const page = {
    context: () => ({
      newPage: () => new Promise((resolve) => (finish = resolve)),
    }),
  };
  assert.equal(
    await extractWithSignedInBrowser(
      page,
      prompt,
      options({ deadlineAt: performance.now() + 20 }),
    ),
    null,
  );
  finish({
    goto: () => assert.fail("late navigation"),
    close: async () => {
      closed += 1;
    },
  });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(closed, 1);
});

test("browser failures are sanitized and never retry", async () => {
  const f = fixture();
  f.child.goto = async () => {
    throw new Error("synthetic private context");
  };
  assert.equal(
    await extractWithSignedInBrowser(f.page, prompt, options()),
    null,
  );
  assert.equal(f.stats().closed, 1);
  assert.equal(f.stats().sent, 0);
});
