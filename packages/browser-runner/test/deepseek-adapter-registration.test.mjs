import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { test } from "node:test";
import { chromium } from "playwright";
import { adapters, measureDeepseek } from "../src/adapters.mjs";

const origin = "https://chat.deepseek.com";
const payload = {
  target_id: "11111111-1111-4111-8111-111111111111",
  account_id: "22222222-2222-4222-8222-222222222222",
  provider: "deepseek",
  model: "fixture-model",
  surface: "consumer_web",
  search_mode: "web_search",
  protocol_version: "synthetic-v1",
  question_set_version: "synthetic-v1",
  question: "Synthetic rainfall question?",
  market: "test-market",
  language: "en",
  scheduled_at: "2026-01-01T00:00:00Z",
  sample_ordinal: 0,
};

test("DeepSeek registers only measurement and requires its complete frozen protocol", async () => {
  assert.deepEqual(adapters.deepseek.operations, ["measure"]);
  for (const operation of ["publish", "lookup", "cleanup"]) {
    assert.equal(
      (await adapters.deepseek.execute(null, operation, payload)).reason,
      "operation_not_supported",
    );
  }
  const invalid = [
    ...Object.keys(payload).map((key) => {
      const value = { ...payload };
      delete value[key];
      return value;
    }),
    { ...payload, provider: "kimi" },
    { ...payload, surface: "model_api" },
    { ...payload, search_mode: "chat" },
    { ...payload, account_id: "fixture-account" },
    { ...payload, sample_ordinal: -1 },
    { ...payload, sample_ordinal: 0.5 },
    { ...payload, scheduled_at: "invalid" },
    { ...payload, question: " " },
    { ...payload, arbitrary: true },
  ];
  for (const value of invalid)
    assert.equal(
      (
        await measureDeepseek({}, value, {
          expectedAccountId: "fixture-account",
        })
      ).reason,
      "invalid_measurement_payload",
    );
  assert.equal(
    (await measureDeepseek({}, payload)).reason,
    "account_identity_unverified",
  );
  assert.equal(
    (
      await measureDeepseek({}, payload, {
        expectedAccountId: "fixture-account",
      })
    ).reason,
    "ownership_unavailable",
  );
  assert.equal(
    (
      await measureDeepseek({}, payload, {
        expectedAccountId: "fixture-account",
        onConversationCaptured() {},
      })
    ).reason,
    "evidence_persistence_unavailable",
  );
});

async function browserFixture(browser, mode) {
  const context = await browser.newContext();
  const calls = [];
  const receipts = [];
  let sends = 0;
  let historyReads = 0;
  let savedSource;
  let sourceHash;
  let creates = 0;
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
          { type: "SEARCH", queries: ["synthetic rainfall"] },
          { type: "THINK", content: "Synthetic private reasoning" },
        ],
        status: "FINISHED",
      },
    }) +
    "event: finish\ndata: [DONE]\n\n";
  await context.route("**/*", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    assert.equal(url.origin, origin, "no external provider calls");
    if (url.pathname === "/") {
      await route.fulfill({
        contentType: "text/html",
        body: `<textarea name="search"></textarea>
          <div role="radio" data-model-type="${payload.model}" aria-checked="true">Fixture model</div>
          <div class="ds-toggle-button" aria-pressed="false">Search</div>
          <div class="ds-toggle-button" aria-pressed="true">DeepThink</div>
          <script>
          localStorage.setItem("userToken", JSON.stringify({__version:"0",value:"synthetic-token"}));
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
          data: {
            biz_code: 0,
            biz_data: {
              id: mode === "identity" ? "other-account" : "fixture-account",
            },
          },
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
              settings: {
                model_configs: {
                  id: 1,
                  value: [
                    {
                      model_type:
                        mode === "model" ? "other-model" : payload.model,
                      enabled: true,
                      switchable: true,
                    },
                  ],
                },
              },
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
      assert.equal(request.postDataJSON().chat_session_id, "fixture-selected");
      await route.fulfill({
        contentType: "text/event-stream; charset=utf-8",
        body: wire,
      });
    } else if (url.pathname === "/api/v0/chat/history_messages") {
      historyReads++;
      assert.equal(url.searchParams.get("chat_session_id"), "fixture-selected");
      if (mode === "history") {
        await route.fulfill({ status: 503, body: "" });
        return;
      }
      await route.fulfill({
        json: {
          code: 0,
          data: {
            biz_code: 0,
            biz_data: {
              cache_control: "REPLACE",
              chat_session: { id: "fixture-selected", current_message_id: 2 },
              chat_messages: [
                {
                  message_id: 1,
                  parent_id: null,
                  role: "USER",
                  status: "FINISHED",
                  has_pending_fragment: false,
                  fragments: [],
                },
                {
                  message_id: 2,
                  parent_id: 1,
                  role: "ASSISTANT",
                  status: "FINISHED",
                  has_pending_fragment: false,
                  fragments: [],
                },
              ],
            },
          },
        },
      });
    } else await route.abort();
  });
  const page = await context.newPage();
  const result = await adapters.deepseek.execute(page, "measure", payload, {
    expectedAccountId: "fixture-account",
    deadlineAt: performance.now() + 10_000,
    sourceDeadlineAt: performance.now() + 8_000,
    onConversationCaptured(receipt) {
      calls.push("ownership");
      receipts.push(receipt);
      return { durable: false };
    },
    async onEvidence(record) {
      calls.push(record.phase);
      if (record.phase === "source") {
        if (mode === "persistence")
          throw new Error("synthetic persistence failure");
        savedSource = JSON.parse(record.source_json);
        sourceHash = record.source_sha256;
        assert.equal(
          sourceHash,
          createHash("sha256").update(record.source_json).digest("hex"),
        );
      }
    },
    async apiExtract() {
      calls.push("extract");
      assert.ok(savedSource, "source was committed before extraction");
      if (mode === "extraction") return null;
      return {
        model: "fixture-parser",
        surface: "model_api",
        extracted: {
          completion: "complete",
          completion_evidence: [{ path: "/messages/1/data/v/status" }],
          search_used: "yes",
          search_evidence: [{ path: "/messages/1/data/v/fragments/1/queries" }],
          answer_segments: [{ path: "/messages/1/data/v/fragments/0/content" }],
          citations: [],
        },
      };
    },
  });
  await context.close();
  return {
    result,
    calls,
    receipts,
    sends,
    historyReads,
    savedSource,
    sourceHash,
  };
}

test("production DeepSeek adapter runs the default browser→history→source→AI flow and fails closed without resubmission", async (t) => {
  const browser = await chromium.launch({
    headless: true,
    executablePath: process.env.GEO_TEST_CHROMIUM_PATH,
  });
  try {
    for (const mode of [
      "success",
      "history",
      "persistence",
      "extraction",
      "identity",
      "model",
    ]) {
      await t.test(mode, async () => {
        const f = await browserFixture(browser, mode);
        if (["success", "history"].includes(mode)) {
          assert.equal(f.result.status, "completed", f.result.reason);
          assert.equal(f.result.raw_answer, "Synthetic answer.");
          assert.deepEqual(f.calls, [
            "ownership",
            "source",
            "extract",
            "candidate",
          ]);
          const official = f.result.evidence.find(
            (item) => item.kind === "official_search_observation",
          );
          assert.equal(official.provider, "deepseek");
          assert.equal(
            official.schema_version,
            "geo.measure.official_search.v4",
          );
          for (const key of [
            "target_id",
            "account_id",
            "model",
            "surface",
            "search_mode",
            "protocol_version",
            "question_set_version",
            "market",
            "language",
            "scheduled_at",
            "sample_ordinal",
          ])
            assert.equal(official[key], payload[key]);
          assert.equal(official.search_event.source_sha256, f.sourceHash);
          assert.deepEqual(f.receipts, [
            {
              provider: "deepseek",
              purpose: "measurement",
              external_conversation_id: "fixture-selected",
            },
          ]);
          assert.equal(
            f.savedSource.ownership.schema_version,
            "geo.deepseek.ownership.v1",
          );
          assert.equal(
            f.savedSource.ownership.creation.chat_session_id,
            "fixture-selected",
          );
          assert.equal(Boolean(f.savedSource.lifecycle), mode === "success");
          assert.equal(
            JSON.stringify(f.savedSource).includes(
              "Synthetic private reasoning",
            ),
            false,
          );
          assert.equal(
            JSON.stringify(f.savedSource).includes("synthetic-token"),
            false,
          );
          const ownership = f.result.evidence.find(
            (item) => item.kind === "provider_conversation_ownership",
          );
          assert.equal(
            ownership.ownership_receipt_status,
            "unpersisted",
            "staging is not independent durability",
          );
        } else {
          assert.notEqual(f.result.status, "completed");
          assert.equal(
            f.result.evidence.some(
              (item) => item.kind === "official_search_observation",
            ),
            false,
          );
          if (mode === "persistence") {
            assert.equal(f.calls.includes("extract"), false);
            assert.equal(f.result.reason, "evidence_persist_failed");
          }
          if (mode === "identity")
            assert.equal(f.result.reason, "account_identity_unverified");
          if (mode === "model")
            assert.equal(f.result.reason, "requested_model_unavailable");
        }
        assert.equal(f.sends, ["identity", "model"].includes(mode) ? 0 : 1);
        assert.equal(
          f.historyReads,
          ["identity", "model"].includes(mode) ? 0 : 1,
        );
      });
    }
  } finally {
    await browser.close();
  }
});
