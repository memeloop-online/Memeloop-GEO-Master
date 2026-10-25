// Shared provider contract: the result envelopes every adapter returns, the
// account-identity helpers they reuse, and the factory that gives a provider
// its frozen shape.
//
// This module is a leaf: it imports nothing but `node:crypto`, so every
// provider module can depend on it without creating an import cycle.
import { createHash } from "node:crypto";

// Fixed platform URLs and source-derived selectors. This connector version is
// not live-verified: no authenticated account was used to prove these flows.
export const CONNECTOR_VERSION = "live_unverified.source_derived.v1";

export function unsupported(reason) {
  return {
    status: "unsupported",
    reason,
    evidence: [],
    connector_version: CONNECTOR_VERSION,
  };
}

export function unknown(reason, stage, evidence = []) {
  return {
    status: "unknown",
    reason,
    stage,
    evidence,
    connector_version: CONNECTOR_VERSION,
  };
}

export function ownIdentity(id, name, avatar) {
  if (
    (typeof id !== "string" && typeof id !== "number") ||
    !String(id).trim() ||
    typeof name !== "string" ||
    !name.trim()
  ) {
    return null;
  }
  return {
    platform_account_id: String(id),
    display_name: name.trim(),
    ...(typeof avatar === "string" && avatar.startsWith("https://")
      ? { avatar_url: avatar }
      : {}),
  };
}

// A temporary Chromium page retains its assigned BrowserContext cookies and
// proxy. Intercept its network request/response before a redirect is followed;
// Playwright route.fetch would use a separate transport, bypassing that proxy.
export async function probeOwnAccount(page, endpoint, extract) {
  const probe = await page.context().newPage();
  let session;
  let onPaused;
  try {
    const expected = new URL(endpoint);
    session = await page.context().newCDPSession(probe);
    onPaused = (event) => {
      const allowed =
        event.request.url === expected.href &&
        (event.responseStatusCode === undefined ||
          event.responseStatusCode === 200);
      void session
        .send(
          allowed ? "Fetch.continueRequest" : "Fetch.failRequest",
          allowed
            ? { requestId: event.requestId }
            : { requestId: event.requestId, errorReason: "BlockedByClient" },
        )
        .catch(() => {});
    };
    session.on("Fetch.requestPaused", onPaused);
    await session.send("Fetch.enable", {
      patterns: [
        { urlPattern: "*", requestStage: "Request" },
        { urlPattern: "*", requestStage: "Response" },
      ],
    });
    const response = await probe.goto(endpoint, {
      waitUntil: "domcontentloaded",
      timeout: 12_000,
    });
    if (!response || response.status() !== 200) return null;
    const actual = new URL(probe.url());
    if (actual.href !== expected.href) return null;
    if (!/application\/json/i.test(response.headers()["content-type"] ?? ""))
      return null;
    const text = await response.text();
    if (text.length > 128_000) return null;
    return extract(JSON.parse(text));
  } catch {
    return null;
  } finally {
    // Closing a stalled navigation first releases the browser's pending
    // network request; otherwise disabling interception can wait indefinitely.
    await probe.close({ timeout: 2_000 }).catch(() => {});
    if (session) {
      if (onPaused) session.off("Fetch.requestPaused", onPaused);
      await session.send("Fetch.disable").catch(() => {});
      await session.detach().catch(() => {});
    }
  }
}

export function digest(title, body) {
  return createHash("sha256")
    .update(`${normalized(title)}\n${normalized(body)}`, "utf8")
    .digest("hex");
}

export function normalized(text) {
  return text.replace(/\s+/gu, " ").trim();
}

export function validMeasurementPayload(payload, provider) {
  return (
    payload !== null &&
    typeof payload === "object" &&
    !Array.isArray(payload) &&
    Object.keys(payload).every((key) =>
      [
        "target_id",
        "account_id",
        "provider",
        "model",
        "surface",
        "search_mode",
        "protocol_version",
        "question_set_version",
        "question",
        "market",
        "language",
        "scheduled_at",
        "sample_ordinal",
      ].includes(key),
    ) &&
    ["target_id", "account_id"].every(
      (key) =>
        typeof payload[key] === "string" &&
        /^[\da-f]{8}-[\da-f]{4}-[\da-f]{4}-[\da-f]{4}-[\da-f]{12}$/iu.test(
          payload[key],
        ),
    ) &&
    payload.provider === provider &&
    payload.surface === "consumer_web" &&
    payload.search_mode === "web_search" &&
    [
      "model",
      "protocol_version",
      "question_set_version",
      "question",
      "market",
      "language",
    ].every(
      (key) =>
        typeof payload[key] === "string" &&
        payload[key].trim().length > 0 &&
        payload[key].length <= 4096,
    ) &&
    typeof payload.scheduled_at === "string" &&
    Number.isFinite(Date.parse(payload.scheduled_at)) &&
    Number.isInteger(payload.sample_ordinal) &&
    payload.sample_ordinal >= 0
  );
}

export function completedMeasurement(payload, observation, capturedEvidence) {
  const observedAt = new Date().toISOString();
  const rawAnswer = observation.raw_answer;
  return {
    status: "completed",
    stage: "official_search_observation",
    occurred_at: observedAt,
    raw_answer: rawAnswer,
    evidence: [
      {
        kind: "official_search_observation",
        schema_version: observation.extraction_audit
          ? "geo.measure.official_search.v4"
          : "geo.measure.official_search.v2",
        target_id: payload.target_id,
        account_id: payload.account_id,
        provider: payload.provider,
        model: payload.model,
        surface: payload.surface,
        search_mode: payload.search_mode,
        protocol_version: payload.protocol_version,
        question_set_version: payload.question_set_version,
        question_sha256: createHash("sha256")
          .update(payload.question, "utf8")
          .digest("hex"),
        market: payload.market,
        language: payload.language,
        scheduled_at: payload.scheduled_at,
        sample_ordinal: payload.sample_ordinal,
        connector_version: CONNECTOR_VERSION,
        provenance: "live",
        disposition: "observed",
        raw_answer: rawAnswer,
        citations: observation.citations,
        search_event: observation.search_event,
      },
      ...(observation.extraction_audit ? [observation.extraction_audit] : []),
      ...capturedEvidence,
    ],
    connector_version: CONNECTOR_VERSION,
  };
}

export function contentPayload(payload, allowUrl = false) {
  if (
    payload === null ||
    typeof payload !== "object" ||
    Array.isArray(payload) ||
    Object.keys(payload).some(
      (key) =>
        key !== "title" &&
        key !== "body" &&
        !(allowUrl && key === "public_url"),
    ) ||
    typeof payload.title !== "string" ||
    typeof payload.body !== "string"
  ) {
    return null;
  }
  const title = payload.title.trim();
  const body = payload.body.trim();
  if (!title || !body || title.length > 150 || body.length > 100_000)
    return null;
  return { title, body };
}

/**
 * Freeze a provider's adapter object without rebuilding it.
 *
 * The caller's literal keeps its own key order, which callers observe through
 * `capabilities()`, so this only validates and freezes. `executeRich` stays
 * absent unless a provider really implements it: `runner.mjs` gates that
 * capability on `typeof adapter.executeRich`.
 */
export function defineProvider(spec) {
  if (spec === null || typeof spec !== "object" || Array.isArray(spec))
    throw new TypeError("provider_spec_invalid");
  if (typeof spec.connectorVersion !== "string" || !spec.connectorVersion)
    throw new TypeError("provider_connector_version_invalid");
  if (
    !Array.isArray(spec.operations) ||
    spec.operations.some((operation) => typeof operation !== "string")
  )
    throw new TypeError("provider_operations_invalid");
  if (typeof spec.identify !== "function")
    throw new TypeError("provider_identify_invalid");
  if (typeof spec.execute !== "function")
    throw new TypeError("provider_execute_invalid");
  for (const key of ["inspectMeasurementOptions", "executeRich"]) {
    if (spec[key] !== undefined && typeof spec[key] !== "function")
      throw new TypeError(`provider_${key}_invalid`);
  }
  if (
    spec.operations.includes("rich_publish") &&
    typeof spec.executeRich !== "function"
  )
    throw new TypeError("provider_rich_publish_requires_execute_rich");
  if (!Object.isFrozen(spec.operations)) Object.freeze(spec.operations);
  return Object.freeze(spec);
}

export function defineRegistry(providers) {
  return Object.freeze(providers);
}
