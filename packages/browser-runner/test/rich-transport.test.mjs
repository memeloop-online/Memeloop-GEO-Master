import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { createServer } from "node:http";
import { after, before, test } from "node:test";
import { setImmediate as nextTurn } from "node:timers/promises";
import { createRunner } from "../src/runner.mjs";
import { createRunnerServer } from "../src/server.mjs";

const OBJECT = "44444444-4444-4444-8444-444444444444";
const ATTEMPT = "66666666-6666-4666-8666-666666666666";
const SESSION = "77777777-7777-4777-8777-777777777777";
const image = Buffer.from("synthetic original image bytes");
const digest = createHash("sha256").update(image).digest("hex");
const serviceToken = "synthetic-callback-service-credential-123";
let callbackServer;
let runnerServer;
let runner;
let base;
let authorizations = 0;
let uploads = 0;
let callbackMode = "grant";
let releaseUpload;
let adapterOutcome = "unsupported";
let captureMode = "grant";
let measurementActions = 0;
const captures = [];
const interpretationCalls = [];
const policyTimings = [];

function canonical(value) {
  if (Array.isArray(value))
    return `[${value.map((item) => canonical(item)).join(",")}]`;
  if (value && typeof value === "object")
    return `{${Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`)
      .join(",")}}`;
  return JSON.stringify(value);
}

function metadata(executionId = "rich-1") {
  const attrs = {
    object_id: OBJECT,
    object_version: 7,
    sha256: digest,
    alt: "Synthetic alternative",
    caption: "",
  };
  const payload = {
    schema_version: 2,
    format: "rich_markdown.v2",
    content_revision_id: "11111111-1111-4111-8111-111111111111",
    policy_version: "deterministic-rich-markdown-v2",
    document: {
      title: "Synthetic title",
      blocks: [
        {
          block_id: "22222222-2222-4222-8222-222222222222",
          kind: "rich",
          text: "",
          citation_ids: [],
          items: [],
          rich: { version: 1, node: { type: "media", attrs } },
        },
      ],
      schema_version: 2,
    },
    media: [
      {
        binding_id: "55555555-5555-4555-8555-555555555555",
        object: {
          object_id: OBJECT,
          object_version: 7,
          sha256: digest,
        },
        media_type: "image/png",
        byte_len: image.length,
        width: 8,
        height: 8,
        alt: attrs.alt,
        caption: attrs.caption,
        role: "image",
      },
    ],
  };
  const variant = { title: payload.document.title, markdown: "Frozen text" };
  const hash = createHash("sha256");
  for (const part of [
    "rich-publication-payload-v2",
    variant.title,
    variant.markdown,
    canonical(payload),
  ]) {
    const bytes = Buffer.from(part);
    const length = Buffer.alloc(8);
    length.writeBigUInt64BE(BigInt(bytes.length));
    hash.update(length).update(bytes);
  }
  variant.payload_hash = hash.digest("hex");
  return {
    schema_version: 1,
    execution_id: executionId,
    attempt_id: ATTEMPT,
    callback_ticket: "abcd",
    variant,
    payload,
  };
}

async function sendRich(value, bytes = image, options = {}) {
  const boundary = "fixture-rich-boundary";
  const field = options.mediaField ?? `media_${OBJECT}_7`;
  const body = Buffer.concat([
    Buffer.from(
      `--${boundary}\r\nContent-Disposition: form-data; name="metadata"\r\nContent-Type: application/json\r\n\r\n${JSON.stringify(value)}\r\n`,
    ),
    Buffer.from(
      `--${boundary}\r\nContent-Disposition: form-data; name="${field}"\r\nContent-Type: ${options.mime ?? "image/png"}\r\n\r\n`,
    ),
    bytes,
    Buffer.from(`\r\n--${boundary}--\r\n`),
  ]);
  const response = await fetch(`${base}/v1/sessions/${SESSION}/execute-rich`, {
    method: "POST",
    headers: {
      authorization: "Bearer internal-runner-token",
      "content-type": `multipart/form-data; boundary=${boundary}`,
    },
    body,
  });
  return { status: response.status, body: await response.json() };
}

before(async () => {
  callbackServer = createServer(async (request, response) => {
    assert.equal(request.headers.authorization, `Bearer ${serviceToken}`);
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks).toString());
    if (request.url.startsWith("/internal/v1/observation-ai/")) {
      interpretationCalls.push({ path: request.url, body });
      assert.deepEqual(Object.keys(body).sort(), [
        "capture_ticket",
        "schema_version",
        "source_capture_id",
        "source_sha256",
      ]);
      assert.equal(
        body.source_capture_id,
        "88888888-8888-4888-8888-888888888888",
      );
      assert.equal(body.source_sha256, captures.at(-1).snapshot.source_sha256);
      response.writeHead(200, { "content-type": "application/json" });
      response.end(
        JSON.stringify(
          request.url.endsWith("/policy")
            ? { prefer_connected_account: false, config_version: 7 }
            : {
                text: '{"completion":"unknown"}',
                model: "actual-project-model",
                config_version: 8,
              },
        ),
      );
      return;
    }
    if (request.url === "/internal/v1/observation-captures") {
      captures.push(body);
      if (captureMode !== "grant") {
        response.writeHead(503).end();
        return;
      }
      response.writeHead(200, { "content-type": "application/json" });
      response.end(
        JSON.stringify({
          capture_id:
            body.ordinal === 0
              ? "88888888-8888-4888-8888-888888888888"
              : "99999999-9999-4999-8999-999999999999",
          schema_version: 1,
          digest_sha256: "a".repeat(64),
          stored_at: new Date().toISOString(),
        }),
      );
      return;
    }
    assert.equal(request.url, "/internal/v1/publication-send/authorize");
    assert.deepEqual(Object.keys(body).sort(), [
      "attempt_id",
      "callback_ticket",
      "payload_hash",
      "runner_session_id",
      "schema_version",
    ]);
    assert.equal(body.runner_session_id, SESSION);
    authorizations++;
    if (callbackMode !== "grant") {
      response.writeHead(409).end();
      return;
    }
    response.writeHead(200, { "content-type": "application/json" });
    response.end(
      JSON.stringify({
        status: "granted",
        attempt_id: body.attempt_id,
        runner_session_id: body.runner_session_id,
        payload_hash: body.payload_hash,
        send_not_after: new Date(Date.now() + 30_000).toISOString(),
      }),
    );
  });
  await new Promise((resolve) =>
    callbackServer.listen(0, "127.0.0.1", resolve),
  );
  const page = {
    url: () => "https://fixture.invalid/",
    async goto() {},
  };
  runner = createRunner({
    browserType: {
      async launch() {
        return {
          async newContext() {
            return {
              async newPage() {
                return page;
              },
              async storageState() {
                return { cookies: [], origins: [] };
              },
              async close() {},
            };
          },
          async close() {},
        };
      },
    },
    platformAdapters: {
      fixture: {
        entry: "https://fixture.invalid/",
        connectorVersion: "fixture.rich.v1",
        operations: ["publish", "rich_publish", "measure"],
        async identify() {
          return { platform_account_id: "fixture-id", display_name: "Fixture" };
        },
        async executeRich(_page, payload, media) {
          uploads++;
          assert.equal(payload.document.title, "Synthetic title");
          assert.deepEqual(media[0].bytes, image);
          if (releaseUpload) await releaseUpload;
          return {
            status: adapterOutcome,
            reason: "fixture_only_no_public_readback",
            evidence: [],
            provenance: "live",
          };
        },
        async execute(_page, operation, _payload, network) {
          assert.equal(operation, "measure");
          measurementActions++;
          await network.onConversationCaptured?.({
            provider: "kimi",
            purpose: "measurement",
            external_conversation_id: "owned-fixture-conversation",
          });
          const source_json = JSON.stringify({
            messages: [{ answer: "synthetic answer" }],
          });
          await network.onEvidence?.({
            kind: "observation_capture",
            schema_version: "geo.observation.capture.v1",
            phase: "source",
            observed_at: new Date().toISOString(),
            source_json,
            source_sha256: createHash("sha256")
              .update(source_json)
              .digest("hex"),
          });
          // The extraction/candidate phase must never run until source ack.
          assert.equal(captures.at(-1)?.snapshot.phase, "source");
          assert.deepEqual(await network.getExtractionPolicy(), {
            prefer_connected_account: false,
            config_version: 7,
          });
          assert.deepEqual(
            await network.apiExtract("must not cross callback boundary"),
            {
              extracted: { completion: "unknown" },
              model: "actual-project-model",
              config_version: 8,
              surface: "model_api",
            },
          );
          await network.onConversationCaptured?.({
            provider: "kimi",
            purpose: "extraction",
            external_conversation_id: "owned-extraction-conversation",
          });
          await network.onEvidence?.({
            kind: "observation_capture",
            schema_version: "geo.observation.capture.v1",
            phase: "extraction",
            source_json,
            source_sha256: createHash("sha256")
              .update(source_json)
              .digest("hex"),
          });
          assert.equal(captures.at(-1)?.snapshot.phase, "extraction");
          const candidate_json = JSON.stringify({ decision: "unverified" });
          await network.onEvidence?.({
            kind: "observation_capture",
            schema_version: "geo.observation.capture.v1",
            phase: "candidate",
            route: "signed_in_browser",
            candidate_json,
            candidate_sha256: createHash("sha256")
              .update(candidate_json)
              .digest("hex"),
            grounding_reason: "unverified",
          });
          return { status: "completed", evidence: [] };
        },
      },
    },
  });
  runnerServer = createRunnerServer({
    token: "internal-runner-token",
    runner,
    callbackOrigin: `http://127.0.0.1:${callbackServer.address().port}`,
    callbackToken: serviceToken,
    captureOrigin: `http://127.0.0.1:${callbackServer.address().port}`,
    captureToken: serviceToken,
    onPolicyTiming: (entry) => policyTimings.push(entry),
  });
  await new Promise((resolve) => runnerServer.listen(0, "127.0.0.1", resolve));
  base = `http://127.0.0.1:${runnerServer.address().port}`;
  const created = await fetch(`${base}/v1/sessions`, {
    method: "POST",
    headers: {
      authorization: "Bearer internal-runner-token",
      "content-type": "application/json",
    },
    body: JSON.stringify({
      session_id: SESSION,
      platform: "fixture",
      storage_state: { cookies: [], origins: [] },
    }),
  });
  assert.equal(created.status, 201);
  const completed = await fetch(`${base}/v1/sessions/${SESSION}/complete`, {
    method: "POST",
    headers: { authorization: "Bearer internal-runner-token" },
  });
  assert.equal(completed.status, 200);
});

after(async () => {
  await runner?.shutdown();
  await new Promise((resolve) => runnerServer?.close(resolve));
  await new Promise((resolve) => callbackServer?.close(resolve));
});

test("rich route authenticates and JSON route cannot bypass binary authorization", async () => {
  const response = await fetch(`${base}/v1/sessions/${SESSION}/execute-rich`, {
    method: "POST",
    headers: { authorization: "Bearer incorrect" },
  });
  assert.equal(response.status, 401);
  const generic = await fetch(`${base}/v1/executions`, {
    method: "POST",
    headers: {
      authorization: "Bearer internal-runner-token",
      "content-type": "application/json",
    },
    body: JSON.stringify({
      execution_id: "rich-bypass",
      session_id: SESSION,
      operation: "rich_publish",
      payload: metadata().payload,
    }),
  });
  assert.deepEqual(await generic.json(), { error: "invalid_execution" });
});

test("exact bytes and grant precede fixture upload; duplicates share outcome", async () => {
  const first = await sendRich(metadata());
  assert.equal(first.status, 200);
  assert.deepEqual(first.body, {
    execution_id: "rich-1",
    connector_version: "fixture.rich.v1",
    provenance: "fixture",
    status: "unsupported",
    reason: "fixture_only_no_public_readback",
    evidence: [],
  });
  assert.equal(authorizations, 1);
  assert.equal(uploads, 1);
  assert.deepEqual(await sendRich(metadata()), first);
  assert.equal(authorizations, 1);
  assert.equal(uploads, 1);
  assert.deepEqual(
    (await sendRich(metadata("rich-1"), Buffer.from("tampered"))).body,
    {
      error: "media_parts_mismatch",
    },
  );
});

test("tampered hash or media metadata never requests send grant", async () => {
  const before = authorizations;
  assert.deepEqual(
    (await sendRich(metadata("bad-media"), image, { mime: "image/jpeg" })).body,
    {
      error: "media_parts_mismatch",
    },
  );
  const changed = metadata("bad-variant");
  changed.variant.payload_hash = "0".repeat(64);
  assert.deepEqual((await sendRich(changed)).body, {
    error: "rich_payload_hash_mismatch",
  });
  assert.equal(authorizations, before);
});

test("unknown media member and metadata over sixteen MiB fail before callback", async () => {
  const before = authorizations;
  assert.deepEqual(
    (
      await sendRich(metadata("extra-media"), image, {
        mediaField: "media_99999999-9999-4999-8999-999999999999_7",
      })
    ).body,
    { error: "media_parts_mismatch" },
  );
  const oversized = metadata("oversized-metadata");
  oversized.variant.markdown = "x".repeat(16 * 1024 * 1024);
  assert.equal((await sendRich(oversized)).status, 400);
  assert.equal(authorizations, before);
});

test("fixture cannot upgrade a rich editor click to completed proof", async () => {
  adapterOutcome = "completed";
  try {
    const result = await sendRich(metadata("no-rich-proof"));
    assert.deepEqual(result.body, {
      status: "unknown",
      reason: "rich_public_readback_unavailable",
      evidence: [],
      execution_id: "no-rich-proof",
      connector_version: "fixture.rich.v1",
      provenance: "fixture",
    });
  } finally {
    adapterOutcome = "unsupported";
  }
});

test("busy reservation blocks a second rich attempt before another grant", async () => {
  let release;
  releaseUpload = new Promise((resolve) => {
    release = resolve;
  });
  const beforeUploads = uploads;
  const beforeGrants = authorizations;
  try {
    const first = sendRich(metadata("busy-first"));
    for (let i = 0; i < 100 && uploads === beforeUploads; i++) await nextTurn();
    assert.equal(uploads, beforeUploads + 1);
    const second = await sendRich(metadata("busy-second"));
    assert.deepEqual(second, { status: 409, body: { error: "session_busy" } });
    assert.equal(authorizations, beforeGrants + 1);
    release();
    assert.equal((await first).status, 200);
  } finally {
    release();
    releaseUpload = undefined;
  }
});

test("failed callback preserves unknown without upload or retry", async () => {
  callbackMode = "deny";
  const before = uploads;
  const result = await sendRich(metadata("denied"));
  assert.equal(result.body.status, "unknown");
  assert.equal(result.body.reason, "send_authorization_unknown");
  assert.equal(uploads, before);
  assert.deepEqual(await sendRich(metadata("denied")), result);
  callbackMode = "grant";
  assert.equal(uploads, before);
});

test("invalid callback deployment configuration fails startup", () => {
  assert.throws(
    () =>
      createRunnerServer({
        token: "runner",
        runner,
        callbackOrigin: "https://example.invalid/extra",
        callbackToken: serviceToken,
      }),
    /invalid_rich_callback_config/,
  );
});

async function sendMeasurement(executionId, ticket = "abcd") {
  const response = await fetch(`${base}/v1/executions`, {
    method: "POST",
    headers: {
      authorization: "Bearer internal-runner-token",
      "content-type": "application/json",
    },
    body: JSON.stringify({
      execution_id: executionId,
      session_id: SESSION,
      operation: "measure",
      payload: {},
      source_capture_ticket: ticket,
    }),
  });
  return { status: response.status, body: await response.json() };
}

test("source and raw extraction checkpoints precede candidate with shared ordered identity", async () => {
  const created = await fetch(`${base}/v1/sessions`, {
    method: "POST",
    headers: {
      authorization: "Bearer internal-runner-token",
      "content-type": "application/json",
    },
    body: JSON.stringify({
      session_id: SESSION,
      platform: "fixture",
      storage_state: { cookies: [], origins: [] },
    }),
  });
  assert.equal(created.status, 201);
  const completed = await fetch(`${base}/v1/sessions/${SESSION}/complete`, {
    method: "POST",
    headers: { authorization: "Bearer internal-runner-token" },
  });
  assert.equal(completed.status, 200);
  const result = await sendMeasurement("capture-1");
  assert.equal(result.body.status, "completed");
  assert.equal(result.body.provenance, "fixture");
  assert.equal(measurementActions, 1);
  assert.equal(captures.length, 3);
  assert.deepEqual(
    policyTimings.map((entry) => entry.stage),
    ["started", "headers", "complete"],
  );
  for (const entry of policyTimings) {
    assert.equal(entry.event, "observation_policy_callback");
    assert.ok(Number.isSafeInteger(entry.elapsed_ms) && entry.elapsed_ms >= 0);
    assert.deepEqual(
      Object.keys(entry).sort(),
      (entry.stage === "headers"
        ? ["event", "stage", "elapsed_ms", "status"]
        : ["event", "stage", "elapsed_ms"]
      ).sort(),
    );
  }
  assert.equal(policyTimings[1].status, 200);
  assert.deepEqual(
    interpretationCalls.map((call) => call.path),
    [
      "/internal/v1/observation-ai/policy",
      "/internal/v1/observation-ai/extract",
    ],
  );
  assert.equal(
    JSON.stringify(interpretationCalls).includes("must not cross"),
    false,
  );
  assert.deepEqual(
    captures.map((capture) => capture.ordinal),
    [0, 1, 2],
  );
  assert.deepEqual(
    captures.map((capture) => capture.snapshot.phase),
    ["source", "extraction", "candidate"],
  );
  assert.deepEqual(Object.keys(captures[0]).sort(), [
    "capture_ticket",
    "observed_at",
    "ordinal",
    "owned_conversation",
    "schema_version",
    "snapshot",
  ]);
  assert.deepEqual(captures[0].owned_conversation, {
    provider: "kimi",
    purpose: "measurement",
    correlation: "create_response",
    external_conversation_id: "owned-fixture-conversation",
  });
  assert.equal(captures[0].snapshot.phase, "source");
  assert.equal(
    captures[1].snapshot.source_capture_id,
    "88888888-8888-4888-8888-888888888888",
  );
  assert.deepEqual(captures[1].owned_conversation, {
    provider: "kimi",
    purpose: "extraction",
    correlation: "create_response",
    external_conversation_id: "owned-extraction-conversation",
  });
  assert.equal(captures[1].snapshot.candidate_json, undefined);
  assert.equal(
    captures[2].snapshot.source_capture_id,
    captures[1].snapshot.source_capture_id,
  );
  assert.deepEqual(await sendMeasurement("capture-1"), result);
  assert.equal(captures.length, 3);
});

test("lost source callback stops extraction and does not retry", async () => {
  captureMode = "fail";
  const before = captures.length;
  const result = await sendMeasurement("capture-failure");
  assert.equal(result.body.status, "unknown");
  assert.equal(captures.length, before + 1);
  assert.equal(captures.at(-1).snapshot.phase, "source");
  assert.deepEqual(await sendMeasurement("capture-failure"), result);
  captureMode = "grant";
});

test("policy timing failures never alter callback results or expose error details", async () => {
  let status = 200;
  const entries = [];
  const callback = createServer((_request, response) => {
    response.writeHead(status, { "content-type": "application/json" });
    response.end(
      JSON.stringify({ prefer_connected_account: false, config_version: 1 }),
    );
  });
  await new Promise((resolve) => callback.listen(0, "127.0.0.1", resolve));
  const server = createRunnerServer({
    token: serviceToken,
    captureOrigin: `http://127.0.0.1:${callback.address().port}`,
    captureToken: serviceToken,
    runner: {
      async execute(_input, _capture, observationAi) {
        return observationAi.policy({ synthetic: "must-not-be-logged" });
      },
    },
    onPolicyTiming: (entry) => {
      entries.push(entry);
      throw new Error("synthetic-private-logger-error");
    },
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  try {
    for (const expected of [200, 503]) {
      status = expected;
      entries.length = 0;
      const response = await fetch(
        `http://127.0.0.1:${server.address().port}/v1/executions`,
        {
          method: "POST",
          headers: {
            authorization: `Bearer ${serviceToken}`,
            "content-type": "application/json",
          },
          body: "{}",
        },
      );
      assert.equal(response.status, expected);
      await response.json();
      assert.deepEqual(
        entries.map((entry) => entry.stage),
        ["started", "headers", expected === 200 ? "complete" : "failed"],
      );
      for (const entry of entries) {
        assert.deepEqual(
          Object.keys(entry).sort(),
          (entry.stage === "headers"
            ? ["event", "stage", "elapsed_ms", "status"]
            : ["event", "stage", "elapsed_ms"]
          ).sort(),
        );
      }
      assert.equal(entries[1].status, expected);
      assert.equal(JSON.stringify(entries).includes("private"), false);
      assert.equal(
        JSON.stringify(entries).includes("must-not-be-logged"),
        false,
      );
    }
  } finally {
    await new Promise((resolve) => server.close(resolve));
    await new Promise((resolve) => callback.close(resolve));
  }
});
