import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { test } from "node:test";
import { chromium } from "playwright";
import { observeDeepSeekWebSearch } from "../src/deepseek-web-search.mjs";

const origin = "https://chat.deepseek.com";
const payload = {
  model: "fixture-model",
  question: "Synthetic rainfall question?",
};
const modelConfigs = [
  { model_type: payload.model, enabled: true, switchable: true },
];
const source = {
  messages: [
    { state: "completed", search: "source found", text: "Synthetic answer." },
  ],
  created_session: {
    chatSessionId: "fixture-selected",
    source: "browser_observed_create",
  },
  received_at: "2026-01-01T00:00:00.000Z",
};
const candidate = {
  completion: "complete",
  completion_evidence: [{ path: "/messages/0/state" }],
  search_used: "yes",
  search_evidence: [{ path: "/messages/0/search" }],
  answer_segments: [{ path: "/messages/0/text" }],
  citations: [],
};
function fixture() {
  const calls = [];
  const page = {
    url: () => `${origin}/`,
    goto: async (url) => {
      calls.push("goto");
      assert.equal(url, `${origin}/`);
    },
    close: () => assert.fail("runner owns page disposal"),
  };
  const options = {
    expectedAccountId: "fixture-account",
    observeConfiguration: (_page, limits) => {
      calls.push("arm-config");
      assert.ok(limits.timeoutMs > 0);
      return {
        result: Promise.resolve({ model_configs: modelConfigs }),
        dispose: () => calls.push("dispose-config"),
      };
    },
    observeCreatedSessions: (_page, limits) => {
      calls.push("arm-create");
      assert.ok(limits.timeoutMs > 0);
      return { dispose: () => calls.push("dispose-create") };
    },
    identify: async () => ({ platform_account_id: "fixture-account" }),
    configure: async (_page, controls) => {
      calls.push("configure");
      assert.equal(controls.searchEnabled, true);
      assert.equal(controls.thinkingEnabled, false);
      return controls;
    },
    capture: async (_page, captureOptions) => {
      calls.push("capture");
      assert.equal(
        Object.hasOwn(captureOptions.binding, "chatSessionId"),
        false,
      );
      await captureOptions.submit(page, captureOptions.signal);
      return source;
    },
    submitQuestion: async () => calls.push("send"),
    prepareSource: (exchange) => ({
      exchange,
      redaction: {
        schema_version: "geo.observation.source-redaction.v1",
        policy: "reasoning-and-private-fields",
        omitted_paths: [],
      },
    }),
    persistConversation: async (receipt) => {
      calls.push("ownership");
      assert.equal(receipt.createdSession, source.created_session);
      assert.equal(receipt.captured, source);
      assert.equal(receipt.expectedAccountId, "fixture-account");
      return { durable: true };
    },
    onEvidence: async (record) => calls.push(record.phase),
    apiExtract: async () => {
      calls.push("extract");
      return {
        extracted: candidate,
        model: "fixture-parser",
        surface: "model_api",
      };
    },
  };
  return { page, calls, options };
}

test("arms both observers before fresh navigation, persists before shared AI extraction, disposes only observers", async () => {
  const f = fixture();
  const result = await observeDeepSeekWebSearch(f.page, payload, f.options);
  assert.equal(result.raw_answer, "Synthetic answer.");
  assert.equal(result.ownership_receipt_status, "persisted");
  assert.deepEqual(f.calls, [
    "arm-config",
    "arm-create",
    "goto",
    "configure",
    "capture",
    "send",
    "ownership",
    "source",
    "extract",
    "candidate",
    "dispose-config",
    "dispose-create",
  ]);
  assert.equal(result.search_event.request_model, payload.model);
  assert.equal(result.extraction_audit.method, "llm_grounded");
  assert.equal(result.source_redaction.omitted_paths.length, 0);
});

test("unavailable ownership or evidence integration is rejected before navigation or sending", async () => {
  for (const [key, reason] of [
    ["persistConversation", "ownership_unavailable"],
    ["onEvidence", "evidence_persistence_unavailable"],
  ]) {
    const f = fixture();
    f.options[key] = undefined;
    assert.deepEqual(
      await observeDeepSeekWebSearch(f.page, payload, f.options),
      { reason },
    );
    assert.deepEqual(f.calls, []);
  }
});

test("missing identity, model or control verification cannot send", async () => {
  for (const scenario of ["identity", "model", "controls", "root"]) {
    const f = fixture();
    if (scenario === "identity") f.options.identify = async () => null;
    if (scenario === "model")
      f.options.observeConfiguration = () => ({
        result: Promise.resolve({ model_configs: [] }),
        dispose() {},
      });
    if (scenario === "controls") f.options.configure = async () => false;
    if (scenario === "root") f.page.url = () => `${origin}/a/chat/s/existing`;
    const result = await observeDeepSeekWebSearch(f.page, payload, f.options);
    assert.ok(result.reason, scenario);
    assert.equal(f.calls.includes("send"), false, scenario);
    assert.ok(f.calls.includes("dispose-create"), scenario);
  }
});

test("failed ownership checkpoint retains source and never invents cleanup authority", async () => {
  const f = fixture();
  f.options.persistConversation = async () => {
    throw new Error("synthetic private ledger details");
  };
  f.options.onConversationCaptured = () =>
    assert.fail("legacy registry must not be called");
  const result = await observeDeepSeekWebSearch(f.page, payload, f.options);
  assert.equal(result.raw_answer, "Synthetic answer.");
  assert.equal(result.ownership_receipt_status, "unpersisted");
  assert.ok(f.calls.indexOf("source") < f.calls.indexOf("extract"));
  assert.equal(f.calls.filter((call) => call === "send").length, 1);
  assert.equal(
    JSON.stringify(result).includes("synthetic private ledger details"),
    false,
  );
  assert.equal(result.completion, undefined);
});

test("source persistence rejection prevents extraction and never resends", async () => {
  const f = fixture();
  f.options.onEvidence = async () => {
    throw new Error("synthetic persistence failure");
  };
  const result = await observeDeepSeekWebSearch(f.page, payload, f.options);
  assert.equal(result.reason, "evidence_persist_failed");
  assert.equal(f.calls.includes("extract"), false);
  assert.equal(f.calls.filter((call) => call === "send").length, 1);
});

test("source checkpoint must resolve before extraction, and unsafe source cannot be persisted", async () => {
  const f = fixture();
  let release;
  let sourceStarted;
  const started = new Promise((resolve) => {
    sourceStarted = resolve;
  });
  f.options.onEvidence = async (record) => {
    f.calls.push(record.phase);
    if (record.phase === "source") {
      sourceStarted();
      await new Promise((resolve) => {
        release = resolve;
      });
    }
  };
  const running = observeDeepSeekWebSearch(f.page, payload, f.options);
  await started;
  assert.equal(f.calls.includes("extract"), false);
  release();
  assert.equal((await running).raw_answer, "Synthetic answer.");

  const unsafe = fixture();
  unsafe.options.prepareSource = () => null;
  const result = await observeDeepSeekWebSearch(
    unsafe.page,
    payload,
    unsafe.options,
  );
  assert.equal(result.reason, "source_privacy_unverified");
  assert.equal(unsafe.calls.includes("source"), false);
  assert.equal(unsafe.calls.includes("ownership"), false);
  assert.equal(unsafe.calls.includes("extract"), false);
  assert.equal(unsafe.calls.filter((call) => call === "send").length, 1);
});

test("a hanging ownership hook leaves time for source persistence", async () => {
  const f = fixture();
  f.options.persistConversation = () => new Promise(() => {});
  const result = await observeDeepSeekWebSearch(f.page, payload, {
    ...f.options,
    deadlineAt: performance.now() + 200,
  });
  assert.equal(result.raw_answer, "Synthetic answer.");
  assert.equal(result.ownership_receipt_status, "unpersisted");
  assert.ok(f.calls.includes("source"));
  assert.equal(f.calls.filter((call) => call === "send").length, 1);
});

test("unknown capture, thrown send and repeated submit callbacks never resend", async () => {
  for (const scenario of ["capture", "send", "duplicate"]) {
    const f = fixture();
    f.options.capture = async (_page, options) => {
      await options.submit(f.page, options.signal);
      if (scenario === "duplicate")
        await options.submit(f.page, options.signal);
      return null;
    };
    if (scenario === "send")
      f.options.submitQuestion = async () => {
        f.calls.push("send");
        throw new Error("synthetic send failed after dispatch");
      };
    const result = await observeDeepSeekWebSearch(f.page, payload, f.options);
    assert.ok(result.reason, scenario);
    assert.equal(f.calls.filter((call) => call === "send").length, 1, scenario);
    assert.equal(f.calls.includes("extract"), false, scenario);
  }
});

test("source deadline bounds navigation, identity, configuration and capture with matching observer TTL", async () => {
  for (const stage of ["goto", "identity", "configuration", "capture"]) {
    const f = fixture();
    const expires = performance.now() + 35;
    const observedTtls = [];
    for (const method of ["observeConfiguration", "observeCreatedSessions"]) {
      const original = f.options[method];
      f.options[method] = (page, limits) => {
        observedTtls.push(limits.timeoutMs);
        assert.ok(limits.timeoutMs <= expires - performance.now() + 2);
        return original(page, limits);
      };
    }
    const hang = () => new Promise(() => {});
    if (stage === "goto") f.page.goto = hang;
    if (stage === "identity") f.options.identify = hang;
    if (stage === "configuration") f.options.configure = hang;
    if (stage === "capture") f.options.capture = hang;
    const result = await observeDeepSeekWebSearch(f.page, payload, {
      ...f.options,
      sourceDeadlineAt: expires,
      deadlineAt: performance.now() + 5000,
    });
    assert.equal(result.reason, "budget_exhausted", stage);
    assert.equal(observedTtls[0], observedTtls[1], stage);
    assert.equal(f.calls.includes("send"), false, stage);
    assert.ok(f.calls.includes("dispose-create"), stage);
  }
});

test("cancellation and expired budgets prevent all sends", async () => {
  const f = fixture();
  const controller = new AbortController();
  controller.abort();
  assert.equal(
    (
      await observeDeepSeekWebSearch(f.page, payload, {
        ...f.options,
        signal: controller.signal,
      })
    ).reason,
    "cancelled",
  );
  assert.equal(
    (
      await observeDeepSeekWebSearch(f.page, payload, {
        ...f.options,
        deadlineAt: performance.now() - 1,
      })
    ).reason,
    "budget_exhausted",
  );
  assert.deepEqual(f.calls, []);
});

test("hanging durable source hook is bounded and cannot start late extraction", async () => {
  const f = fixture();
  let release;
  f.options.onEvidence = () =>
    new Promise((resolve) => {
      release = resolve;
    });
  const result = await observeDeepSeekWebSearch(f.page, payload, {
    ...f.options,
    deadlineAt: performance.now() + 40,
  });
  assert.equal(result.reason, "budget_exhausted");
  release();
  await new Promise(setImmediate);
  assert.equal(f.calls.includes("extract"), false);
  assert.equal(f.calls.filter((call) => call === "send").length, 1);
});

test("real Chrome runs fresh navigation, dynamic controls, exact prefetched session, capture and source-first AI parsing", async () => {
  const browser = await chromium.launch({
    headless: true,
    executablePath: process.env.GEO_TEST_CHROMIUM_PATH,
  });
  const calls = [];
  let sends = 0;
  let creates = 0;
  let savedSource;
  let savedSourceHash;
  const event = (name, data) =>
    `event: ${name}\ndata: ${JSON.stringify(data)}\n\n`;
  const wire =
    event("ready", {
      request_message_id: 1,
      response_message_id: 2,
      model_type: payload.model,
    }) +
    event("message", {
      p: "response",
      o: "SET",
      v: {
        fragments: [
          { type: "RESPONSE", content: "Synthetic answer." },
          {
            type: "SEARCH",
            results: [
              { url: "https://example.org/source", title: "Synthetic source" },
            ],
            queries: ["synthetic rainfall"],
          },
          {
            type: "THINK",
            content: "Synthetic private reasoning must not persist.",
          },
        ],
        status: "FINISHED",
      },
    }) +
    "event: finish\ndata: [DONE]\n\n";
  try {
    const context = await browser.newContext();
    await context.route("**/*", async (route) => {
      const request = route.request();
      const url = new URL(request.url());
      assert.equal(url.origin, origin);
      if (url.pathname === "/") {
        await route.fulfill({
          contentType: "text/html",
          body: `<textarea name="search"></textarea>
            <div role="radio" data-model-type="${payload.model}" aria-checked="true">Fixture model</div>
            <div class="ds-toggle-button" aria-pressed="false">Search</div>
            <div class="ds-toggle-button" aria-pressed="true">DeepThink</div>
            <script>
            localStorage.setItem("userToken",JSON.stringify({__version:"0",value:"synthetic-token"}));
            const prefetched = Promise.all([0,1].map(() =>
              fetch("/api/v0/chat_session/create",{method:"POST",body:"{}"}).then(r=>r.json())));
            fetch("/api/v0/client/settings?scope=model");
            document.querySelectorAll(".ds-toggle-button").forEach(node => node.onclick = () =>
              node.setAttribute("aria-pressed",String(node.getAttribute("aria-pressed") !== "true")));
            document.querySelector("textarea").onkeydown = async event => {
              if(event.key !== "Enter") return;
              event.preventDefault();
              const session = (await prefetched)[0].data.biz_data.chat_session.id;
              history.pushState({},"","/a/chat/s/"+session);
              fetch("/api/v0/chat/completion",{
                method:"POST",headers:{"content-type":"application/json"},
                body:JSON.stringify({
                  chat_session_id:session,parent_message_id:null,model_type:"${payload.model}",
                  prompt:document.querySelector("textarea").value,ref_file_ids:[],
                  search_enabled:document.querySelectorAll(".ds-toggle-button")[0].getAttribute("aria-pressed") === "true",
                  thinking_enabled:document.querySelectorAll(".ds-toggle-button")[1].getAttribute("aria-pressed") === "true"
                })
              }).then(r=>r.text());
            };
            </script>`,
        });
      } else if (url.pathname === "/api/v0/users/current") {
        await route.fulfill({
          json: {
            code: 0,
            data: { biz_code: 0, biz_data: { id: "fixture-account" } },
          },
        });
      } else if (url.pathname === "/api/v0/client/settings") {
        await route.fulfill({
          json: {
            code: 0,
            data: {
              biz_code: 0,
              biz_data: {
                version: 1,
                settings: { model_configs: { id: 1, value: modelConfigs } },
              },
            },
          },
        });
      } else if (url.pathname === "/api/v0/chat_session/create") {
        creates++;
        await route.fulfill({
          json: {
            code: 0,
            data: {
              biz_code: 0,
              biz_data: {
                chat_session: {
                  id: creates === 1 ? "fixture-selected" : "fixture-unselected",
                },
              },
            },
          },
        });
      } else if (url.pathname === "/api/v0/chat/completion") {
        sends++;
        assert.equal(
          request.postDataJSON().chat_session_id,
          "fixture-selected",
        );
        await route.fulfill({
          contentType: "text/event-stream; charset=utf-8",
          body: wire,
        });
      } else await route.abort();
    });
    const page = await context.newPage();
    const result = await observeDeepSeekWebSearch(page, payload, {
      expectedAccountId: "fixture-account",
      deadlineAt: performance.now() + 10_000,
      sourceDeadlineAt: performance.now() + 8_000,
      persistConversation: async ({
        createdSession,
        captured,
        binding,
        expectedAccountId,
      }) => {
        calls.push("ownership");
        assert.equal(createdSession.chatSessionId, "fixture-selected");
        assert.equal(createdSession.source, "browser_observed_create");
        assert.equal(expectedAccountId, "fixture-account");
        assert.equal(
          JSON.stringify(captured).includes("Synthetic private reasoning"),
          false,
        );
        assert.equal(binding.model, payload.model);
        assert.equal(binding.searchEnabled, true);
        assert.equal(binding.thinkingEnabled, false);
        assert.match(binding.questionSha256, /^[a-f0-9]{64}$/u);
        return { durable: true };
      },
      onEvidence: async (record) => {
        calls.push(record.phase);
        if (record.phase === "source") {
          savedSource = JSON.parse(record.source_json);
          savedSourceHash = record.source_sha256;
          assert.equal(
            savedSourceHash,
            createHash("sha256").update(record.source_json).digest("hex"),
          );
        }
      },
      apiExtract: async (prompt) => {
        calls.push("extract");
        assert.ok(savedSource, "source checkpoint completed before extraction");
        assert.ok(
          prompt.includes(
            `/source_redaction = ${JSON.stringify(savedSource.source_redaction)}`,
          ),
          "extraction receives the persisted privacy metadata",
        );
        for (const [index, message] of savedSource.messages.entries())
          assert.ok(
            prompt.includes(`/messages/${index} = ${JSON.stringify(message)}`),
          );
        assert.equal(prompt.includes("Synthetic private reasoning"), false);
        assert.equal(
          savedSource.messages[1].data.v.fragments[0].content,
          "Synthetic answer.",
        );
        return {
          model: "fixture-parser",
          surface: "model_api",
          extracted: {
            completion: "complete",
            completion_evidence: [{ path: "/messages/1/data/v/status" }],
            search_used: "yes",
            search_evidence: [
              { path: "/messages/1/data/v/fragments/1/results" },
            ],
            answer_segments: [
              { path: "/messages/1/data/v/fragments/0/content" },
            ],
            citations: [],
          },
        };
      },
    });
    assert.equal(result.raw_answer, "Synthetic answer.");
    assert.equal(result.ownership_receipt_status, "persisted");
    assert.deepEqual(calls, ["ownership", "source", "extract", "candidate"]);
    assert.equal(sends, 1);
    assert.equal(creates, 2);
    assert.equal(page.isClosed(), false);
    assert.equal(page.listenerCount("response"), 0);
    assert.equal(page.listenerCount("request"), 0);
    assert.equal(page.listenerCount("framenavigated"), 0);
    assert.equal(
      JSON.stringify(savedSource).includes("synthetic-token"),
      false,
    );
    assert.equal(
      JSON.stringify(savedSource).includes("Synthetic private reasoning"),
      false,
    );
    assert.ok(result.source_redaction.omitted_paths.length > 0);
    assert.deepEqual(savedSource.source_redaction, result.source_redaction);
    assert.equal(result.extraction_audit.source_sha256, savedSourceHash);
    await context.close();
  } finally {
    await browser.close();
  }
});
