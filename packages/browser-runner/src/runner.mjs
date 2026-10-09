import { createHash } from "node:crypto";
import { setTimeout as delay } from "node:timers/promises";
import { chromium } from "playwright";
import { adapters as defaultAdapters } from "./adapters.mjs";
import { parseExtractionJson } from "./ai-observation-parser.mjs";
import { createLinuxDesktopRuntime } from "./interactive-desktop.mjs";
import {
  recoverKimiConversation,
  deleteKimiConversation,
} from "./provider-conversation-cleanup.mjs";
import {
  stageRichPublication,
  executeAuthorizedRichPublication,
  RichPublicationError,
} from "./rich-publication.mjs";

const ID = /^[a-zA-Z0-9_-]{1,128}$/;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const SHA256 = /^[0-9a-f]{64}$/;
const VIEWPORT = Object.freeze({ width: 1280, height: 800 });
const OPERATIONS = new Set(["publish", "measure", "lookup", "rich_publish"]);
const JSON_OPERATIONS = new Set(["publish", "measure", "lookup"]);
const BROWSER_CHANNELS = new Set(["chromium", "chrome", "msedge"]);
const STATUSES = new Set([
  "unsupported",
  "login_required",
  "challenge",
  "unknown",
  "completed",
]);
// Match the existing page-navigation budget, within the gateway's 60s request
// timeout. Website-managed renewal can finish after initial DOM hydration.
const BROWSER_READINESS_TIMEOUT_MS = 30_000;
const RESTORED_KIMI_IDENTITY_WAIT_MS = BROWSER_READINESS_TIMEOUT_MS;

export class RunnerError extends Error {
  constructor(status, code) {
    super(code);
    this.status = status;
    this.code = code;
  }
}

function object(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function fields(value, permitted) {
  return (
    object(value) &&
    Object.keys(value).every((field) => permitted.includes(field))
  );
}

function validId(value) {
  return typeof value === "string" && ID.test(value);
}

function parseBrowserChannel(value) {
  if (value === undefined) return undefined;
  if (typeof value !== "string" || !BROWSER_CHANNELS.has(value)) {
    throw new Error("invalid_browser_channel");
  }
  return value;
}

function parseProxy(proxy) {
  if (proxy === undefined) return undefined;
  if (
    !fields(proxy, ["server", "username", "password"]) ||
    typeof proxy.server !== "string" ||
    !/^(https?:\/\/|socks5:\/\/)[^\s]+$/.test(proxy.server) ||
    (proxy.username !== undefined && typeof proxy.username !== "string") ||
    (proxy.password !== undefined && typeof proxy.password !== "string")
  ) {
    throw new RunnerError(400, "invalid_proxy");
  }
  return proxy;
}

function parseState(state) {
  if (state === undefined) return undefined;
  // Playwright accepts a JSON object here. Never accept a filename/URL from callers.
  if (
    !fields(state, ["cookies", "origins"]) ||
    !Array.isArray(state.cookies) ||
    !Array.isArray(state.origins)
  ) {
    throw new RunnerError(400, "invalid_storage_state");
  }
  return state;
}

function validateIdentity(identity) {
  return (
    fields(identity, ["platform_account_id", "display_name", "avatar_url"]) &&
    typeof identity.platform_account_id === "string" &&
    identity.platform_account_id.length > 0 &&
    typeof identity.display_name === "string" &&
    identity.display_name.length > 0 &&
    (identity.avatar_url === undefined ||
      typeof identity.avatar_url === "string")
  );
}

function isChallenge(page) {
  // Only use a conservative signal. Do not automate challenge solving.
  return /\/(captcha|challenge|verify)(\/|[?#]|$)/i.test(
    new URL(page.url()).pathname,
  );
}

export function createRunner(options = {}) {
  const {
    browserType = chromium,
    browserChannel = process.env.GEO_BROWSER_CHANNEL,
    interactiveRuntime = process.env.GEO_BROWSER_INTERACTIVE_RUNTIME,
    desktopRuntime,
    restoredKimiIdentityWaitMs = RESTORED_KIMI_IDENTITY_WAIT_MS,
    sessionIdleMs = 15 * 60_000,
    executionRetentionMs = 5 * 60_000,
    executionTimeoutMs = 120_000,
    measurementExecutionTimeoutMs = 240_000,
    measurementSourceTimeoutMs = 120_000,
    maintenanceIntervalMs = 30_000,
    clock = () => Date.now(),
  } = options;
  // Only the installed browser and adapters are a live execution path. Tests
  // and other injected components cannot upgrade their own receipt provenance.
  const injected =
    Object.hasOwn(options, "platformAdapters") ||
    Object.hasOwn(options, "browserType");
  const provenance = injected ? "fixture" : "live";
  const platformAdapters =
    injected && Object.hasOwn(options, "platformAdapters")
      ? options.platformAdapters
      : defaultAdapters;
  const selectedBrowserChannel = parseBrowserChannel(browserChannel);
  if (interactiveRuntime !== undefined && interactiveRuntime !== "linux-vnc")
    throw new Error("invalid_interactive_runtime");
  const desktop =
    interactiveRuntime === "linux-vnc"
      ? (desktopRuntime ?? createLinuxDesktopRuntime())
      : null;
  if (
    ![
      sessionIdleMs,
      executionRetentionMs,
      executionTimeoutMs,
      measurementExecutionTimeoutMs,
      measurementSourceTimeoutMs,
      maintenanceIntervalMs,
      restoredKimiIdentityWaitMs,
    ].every((value) => Number.isFinite(value) && value > 0) ||
    restoredKimiIdentityWaitMs > RESTORED_KIMI_IDENTITY_WAIT_MS
  ) {
    throw new Error("invalid_runner_retention");
  }
  const sessions = new Map();
  const pendingSessions = new Set();
  const executions = new Map();
  let browserPromise;
  let reapingPromise;

  function dispose(record) {
    record.lifetime.abort();
    for (const client of record.desktopClients) client.terminate();
    record.desktopClients.clear();
    return record.desktop ? record.desktop.close() : record.context.close();
  }

  function capabilities() {
    return {
      connectors: Object.entries(platformAdapters)
        .filter(
          ([platform, adapter]) =>
            /^[a-z][a-z0-9_]{0,63}$/.test(platform) &&
            adapter &&
            typeof adapter.connectorVersion === "string" &&
            adapter.connectorVersion.length > 0 &&
            adapter.connectorVersion.length <= 100 &&
            Array.isArray(adapter.operations) &&
            adapter.operations.every((operation) => OPERATIONS.has(operation)),
        )
        .map(([platform, adapter]) => ({
          platform,
          placement_slot: "primary",
          connector_version: adapter.connectorVersion,
          operations: adapter.operations.filter(
            (operation) =>
              operation !== "rich_publish" ||
              typeof adapter.executeRich === "function",
          ),
          // Presence in a deployed runner is not live account verification.
          verified: false,
        })),
    };
  }

  async function browser() {
    if (!browserPromise) {
      browserPromise = browserType
        .launch({
          headless: true,
          ...(selectedBrowserChannel
            ? { channel: selectedBrowserChannel }
            : {}),
        })
        .catch((error) => {
          browserPromise = undefined;
          throw error;
        });
    }
    return browserPromise;
  }

  function session(id) {
    const found = sessions.get(id);
    if (!found) throw new RunnerError(404, "session_not_found");
    if (found.desktop?.closed) {
      sessions.delete(id);
      throw new RunnerError(404, "session_not_found");
    }
    if (!found.busy && clock() - found.lastTouched >= sessionIdleMs) {
      sessions.delete(id);
      void dispose(found).catch(() => {});
      throw new RunnerError(404, "session_not_found");
    }
    found.lastTouched = clock();
    return found;
  }

  async function reap() {
    if (reapingPromise) return reapingPromise;
    reapingPromise = (async () => {
      const now = clock();
      const expired = [];
      for (const [id, record] of sessions) {
        if (
          record.desktop?.closed ||
          (!record.busy && now - record.lastTouched >= sessionIdleMs)
        ) {
          sessions.delete(id);
          expired.push(dispose(record));
        }
      }
      for (const [id, entry] of executions) {
        if (
          entry.settledAt !== null &&
          now - entry.settledAt >= executionRetentionMs
        ) {
          executions.delete(id);
        }
      }
      await Promise.allSettled(expired);
    })();
    try {
      await reapingPromise;
    } finally {
      reapingPromise = undefined;
    }
  }
  const maintenance = setInterval(() => {
    void reap().catch(() => {});
  }, maintenanceIntervalMs);
  maintenance.unref?.();

  async function phase(record) {
    if (isChallenge(record.page)) return "challenge";
    if (!record.identity) {
      let identity;
      try {
        identity = await record.adapter.identify(record.page);
      } catch {
        // An identity probe failure never proves account connection.
        return "login_required";
      }
      if (validateIdentity(identity)) record.identity = identity;
    }
    return record.identity
      ? record.completed
        ? "connected"
        : "ready_to_complete"
      : "login_required";
  }

  async function create(input) {
    if (
      !fields(input, ["session_id", "platform", "storage_state", "proxy"]) ||
      !validId(input.session_id) ||
      typeof input.platform !== "string"
    ) {
      throw new RunnerError(400, "invalid_session");
    }
    const adapter = platformAdapters[input.platform];
    if (!adapter) throw new RunnerError(400, "unsupported_platform");
    if (
      sessions.has(input.session_id) ||
      pendingSessions.has(input.session_id)
    ) {
      throw new RunnerError(409, "session_exists");
    }
    const proxy = parseProxy(input.proxy);
    const storageState = parseState(input.storage_state);
    if (!desktop && !storageState) {
      throw new RunnerError(503, "capability_missing");
    }
    pendingSessions.add(input.session_id);
    let context;
    let desktopSession;
    try {
      let page;
      if (desktop && !storageState) {
        desktopSession = await desktop.open({
          browserType,
          browserChannel: selectedBrowserChannel,
          proxy,
          storageState,
          viewport: VIEWPORT,
        });
        ({ context, page } = desktopSession);
      } else {
        context = await (
          await browser()
        ).newContext({
          viewport: VIEWPORT,
          ...(proxy ? { proxy } : {}),
          ...(storageState ? { storageState } : {}),
        });
        page = await context.newPage();
      }
      // The caller cannot provide navigation targets. OAuth redirects, if any,
      // are handled by the platform's own UI in this isolated context.
      await page.goto(adapter.entry, {
        waitUntil: "domcontentloaded",
        timeout: BROWSER_READINESS_TIMEOUT_MS,
      });
      const record = {
        platform: input.platform,
        adapter,
        // Snapshot host configuration before executing any adapter code.
        connectorVersion: adapter.connectorVersion,
        context,
        page,
        desktop: desktopSession,
        desktopClients: new Set(),
        proxy,
        restoredKimi: input.platform === "kimi" && !!storageState,
        lifetime: new AbortController(),
        identity: null,
        completed: false,
        busy: false,
        lastTouched: clock(),
      };
      sessions.set(input.session_id, record);
      return { session_id: input.session_id, phase: await phase(record) };
    } catch (error) {
      if (desktopSession) await desktopSession.close();
      else if (context) await context.close();
      throw error;
    } finally {
      pendingSessions.delete(input.session_id);
    }
  }

  async function restoredIdentityProbe(record, deadline) {
    const { signal } = record.lifetime;
    if (signal.aborted) throw new RunnerError(404, "session_not_found");
    let timer;
    let onAbort;
    const interrupted = new Promise((_, reject) => {
      onAbort = () => reject(new RunnerError(404, "session_not_found"));
      signal.addEventListener("abort", onAbort, { once: true });
      timer = setTimeout(
        () => reject(new RunnerError(503, "identity_probe_timeout")),
        Math.max(1, deadline - performance.now()),
      );
    });
    try {
      // A hung/failed browser probe is infrastructure failure, not proof that
      // credentials expired. Never retry exceptions or invoke token APIs.
      return await Promise.race([
        record.adapter.identify(record.page),
        interrupted,
      ]);
    } finally {
      clearTimeout(timer);
      signal.removeEventListener("abort", onAbort);
    }
  }

  async function identityForCompletion(record) {
    // Restored Kimi storage may need the website's hydration/refresh before
    // its saved access token verifies. Wait only for a missing identity, on
    // the existing page, regardless of the runner's desktop/headless mode.
    const deadline = performance.now() + restoredKimiIdentityWaitMs;
    for (;;) {
      if (isChallenge(record.page)) throw new RunnerError(409, "challenge");
      const identity = record.restoredKimi
        ? await restoredIdentityProbe(record, deadline)
        : await record.adapter.identify(record.page);
      if (validateIdentity(identity)) {
        if (
          record.identity &&
          record.identity.platform_account_id !== identity.platform_account_id
        )
          throw new RunnerError(409, "account_mismatch");
        return identity;
      }
      if (!record.restoredKimi) throw new RunnerError(409, "login_required");
      // The adapter's missing-identity result can also mean a failed request
      // or unfinished website renewal. A deadline cannot prove login expiry.
      if (performance.now() >= deadline)
        throw new RunnerError(503, "identity_not_ready");
      try {
        await delay(
          Math.min(250, Math.max(1, deadline - performance.now())),
          undefined,
          {
            signal: record.lifetime.signal,
          },
        );
      } catch (error) {
        if (record.lifetime.signal.aborted)
          throw new RunnerError(404, "session_not_found");
        throw error;
      }
    }
  }

  async function complete(id) {
    const record = session(id);
    if (record.busy) throw new RunnerError(409, "session_busy");
    if (record.desktop) {
      // Disconnect remote input before saving state, then recheck identity
      // after input is no longer possible.
      record.busy = true;
      let revokingInput = false;
      try {
        const identity = await identityForCompletion(record);
        revokingInput = true;
        for (const client of record.desktopClients) client.terminate();
        record.desktopClients.clear();
        await record.desktop.closeInput();
        const finalIdentity = await record.adapter.identify(record.page);
        if (
          !validateIdentity(finalIdentity) ||
          finalIdentity.platform_account_id !== identity.platform_account_id
        )
          throw new RunnerError(409, "account_mismatch");
        const storage_state = await record.context.storageState();
        record.identity = finalIdentity;
        record.completed = true;
        return { identity: finalIdentity, storage_state };
      } catch (error) {
        // An ordinary incomplete-login probe must not strand the user.
        if (revokingInput || record.desktop.closed) {
          sessions.delete(id);
          await dispose(record).catch(() => {});
        }
        throw error;
      } finally {
        record.busy = false;
      }
    }
    // A restored headless session uses the same bounded readiness path above.
    if (record.restoredKimi) record.busy = true;
    try {
      const identity = await identityForCompletion(record);
      record.identity = identity;
      record.completed = true;
      return { identity, storage_state: await record.context.storageState() };
    } finally {
      if (record.restoredKimi) record.busy = false;
    }
  }

  // Trusted gateway only. Never serialize this value in runner HTTP responses.
  function desktopEndpoint(id) {
    const record = session(id);
    if (!record.desktop || record.completed || record.busy)
      throw new RunnerError(409, "desktop_unavailable");
    try {
      return record.desktop.endpoint();
    } catch {
      throw new RunnerError(409, "desktop_unavailable");
    }
  }

  function attachDesktopClient(id, client) {
    const endpoint = desktopEndpoint(id);
    const record = session(id);
    record.desktopClients.add(client);
    client.once("close", () => record.desktopClients.delete(client));
    return endpoint;
  }

  async function status(id) {
    const record = session(id);
    return {
      phase: await phase(record),
      ...(record.identity ? { identity: record.identity } : {}),
    };
  }

  async function measurementOptions(id) {
    const record = session(id);
    if (record.busy) throw new RunnerError(409, "session_busy");
    if (!record.completed) throw new RunnerError(409, "login_required");
    if (!record.adapter.inspectMeasurementOptions)
      throw new RunnerError(422, "measurement_options_unavailable");
    record.busy = true;
    try {
      const identity = await record.adapter.identify(record.page);
      if (
        !validateIdentity(identity) ||
        identity.platform_account_id !== record.identity.platform_account_id
      )
        throw new RunnerError(409, "account_mismatch");
      const result = await record.adapter.inspectMeasurementOptions(
        record.page,
      );
      if (!result)
        throw new RunnerError(422, "measurement_options_unavailable");
      return result;
    } finally {
      record.busy = false;
    }
  }

  async function execute(input, persistCapture, observationAi) {
    if (
      !fields(input, [
        "execution_id",
        "session_id",
        "operation",
        "payload",
        "source_capture_ticket",
      ]) ||
      !validId(input.execution_id) ||
      !validId(input.session_id) ||
      !JSON_OPERATIONS.has(input.operation) ||
      !object(input.payload) ||
      (input.source_capture_ticket !== undefined &&
        (input.operation !== "measure" ||
          typeof input.source_capture_ticket !== "string" ||
          !/^[0-9a-f]{2,4096}$/.test(input.source_capture_ticket)))
    ) {
      throw new RunnerError(400, "invalid_execution");
    }
    // Do not retain plaintext content in the in-process duplicate cache.
    const fingerprint = createHash("sha256")
      .update(JSON.stringify(input))
      .digest("hex");
    const previous = executions.get(input.execution_id);
    if (previous) {
      if (previous.fingerprint !== fingerprint)
        throw new RunnerError(409, "execution_conflict");
      return previous.promise;
    }
    const record = session(input.session_id);
    const hostReceipt = {
      execution_id: input.execution_id,
      provenance,
      ...(typeof record.connectorVersion === "string"
        ? { connector_version: record.connectorVersion }
        : {}),
    };
    if (!record.completed) throw new RunnerError(409, "login_required");
    if (input.source_capture_ticket !== undefined && !persistCapture)
      return {
        status: "unsupported",
        reason: "observation_capture_unavailable",
        evidence: [],
        ...hostReceipt,
      };
    if (!record.adapter.operations?.includes(input.operation)) {
      return {
        status: "unsupported",
        reason: "operation_not_supported",
        evidence: [],
        ...hostReceipt,
      };
    }
    if (record.busy) throw new RunnerError(409, "session_busy");
    if (isChallenge(record.page)) {
      return {
        status: "challenge",
        evidence: [],
        ...hostReceipt,
      };
    }
    // Reserve this context before the asynchronous identity probe; otherwise
    // two different execution IDs can both pass the busy check and submit.
    record.busy = true;
    const entry = { fingerprint, promise: null, settledAt: null };
    const promise = (async () => {
      const deadline = Symbol("execution_deadline");
      const startedAt = performance.now();
      const timeoutMs =
        input.operation === "measure"
          ? measurementExecutionTimeoutMs
          : executionTimeoutMs;
      const deadlineAt = startedAt + timeoutMs;
      const sourceDeadlineAt =
        input.operation === "measure"
          ? Math.min(deadlineAt, startedAt + measurementSourceTimeoutMs)
          : undefined;
      const controller = new AbortController();
      let timer;
      const timeout = new Promise((resolve) => {
        const expire = () => {
          const remaining = deadlineAt - performance.now();
          if (remaining > 0) {
            // Node timers may fire just before a fractional monotonic deadline.
            timer = setTimeout(expire, Math.ceil(remaining));
            return;
          }
          controller.abort();
          resolve(deadline);
        };
        timer = setTimeout(expire, timeoutMs);
      });
      function timedOut() {
        // The external side effect may have happened. Closing this browser
        // context prevents a stalled adapter from issuing later actions;
        // Rust must reconcile before any new attempt.
        if (sessions.get(input.session_id) === record) {
          sessions.delete(input.session_id);
        }
        void dispose(record).catch(() => {});
        return {
          status: "unknown",
          reason: "execution_deadline",
          evidence: [],
          ...hostReceipt,
        };
      }
      try {
        const currentIdentity = await Promise.race([
          record.adapter.identify(record.page).catch(() => null),
          timeout,
        ]);
        if (currentIdentity === deadline) return timedOut();
        if (
          !validateIdentity(currentIdentity) ||
          currentIdentity.platform_account_id !==
            record.identity.platform_account_id
        ) {
          return {
            status: "login_required",
            reason: "account_identity_unverified",
            evidence: [],
            ...hostReceipt,
          };
        }
        // Adapters own fixed, typed platform actions; user payload is never script
        // or navigation, and adapter evidence must not be inferred from click success.
        let sourceReceipt = null;
        let sourceDigest = null;
        let sourceOwnership = null;
        let extractionOwnership = null;
        let candidateOrdinal = 0;
        function captureReceipt(receipt) {
          if (
            !receipt ||
            receipt.schema_version !== 1 ||
            !UUID.test(receipt.capture_id) ||
            !SHA256.test(receipt.digest_sha256) ||
            typeof receipt.stored_at !== "string" ||
            !Number.isFinite(Date.parse(receipt.stored_at))
          )
            throw new Error("capture_receipt_invalid");
          return receipt;
        }
        const captureHooks =
          input.source_capture_ticket === undefined
            ? {}
            : {
                async getExtractionPolicy({ signal } = {}) {
                  if (!sourceReceipt || !observationAi?.policy)
                    throw new Error("observation_policy_unavailable");
                  const policy = await observationAi.policy(
                    {
                      schema_version: 1,
                      capture_ticket: input.source_capture_ticket,
                      source_capture_id: sourceReceipt.capture_id,
                      source_sha256: sourceDigest,
                    },
                    { signal },
                  );
                  if (
                    typeof policy?.prefer_connected_account !== "boolean" ||
                    !Number.isSafeInteger(policy.config_version) ||
                    policy.config_version < 0
                  )
                    throw new Error("observation_policy_invalid");
                  return policy;
                },
                async apiExtract(_prompt, { signal } = {}) {
                  if (!sourceReceipt || !observationAi?.extract) return null;
                  const result = await observationAi.extract(
                    {
                      schema_version: 1,
                      capture_ticket: input.source_capture_ticket,
                      source_capture_id: sourceReceipt.capture_id,
                      source_sha256: sourceDigest,
                    },
                    { signal },
                  );
                  if (
                    typeof result?.text !== "string" ||
                    Buffer.byteLength(result.text) > 150_000 ||
                    typeof result.model !== "string" ||
                    !/^[\w./:-]{1,256}$/u.test(result.model) ||
                    !Number.isSafeInteger(result.config_version) ||
                    result.config_version < 0
                  )
                    return null;
                  const extracted = parseExtractionJson(result.text);
                  return extracted
                    ? {
                        extracted,
                        model: result.model,
                        config_version: result.config_version,
                        surface: "model_api",
                      }
                    : null;
                },
                onConversationCaptured(receipt) {
                  if (
                    receipt?.provider === "kimi" &&
                    ["measurement", "extraction"].includes(receipt.purpose) &&
                    validId(receipt.external_conversation_id)
                  ) {
                    if (receipt.purpose === "measurement")
                      sourceOwnership = receipt;
                    else extractionOwnership = receipt;
                  }
                  // Identification is not a durable write. The evidence
                  // callback below must commit before any capture is acknowledged.
                  return { durable: false };
                },
                async onEvidence(evidence) {
                  if (
                    evidence?.kind !== "observation_capture" ||
                    evidence.schema_version !== "geo.observation.capture.v1"
                  )
                    throw new Error("capture_evidence_invalid");
                  if (evidence.phase === "source") {
                    if (
                      sourceReceipt ||
                      typeof evidence.source_json !== "string" ||
                      !SHA256.test(evidence.source_sha256) ||
                      createHash("sha256")
                        .update(evidence.source_json)
                        .digest("hex") !== evidence.source_sha256
                    )
                      throw new Error("source_capture_invalid");
                    const body = {
                      schema_version: 1,
                      capture_ticket: input.source_capture_ticket,
                      ordinal: 0,
                      observed_at:
                        evidence.observed_at ?? new Date(clock()).toISOString(),
                      snapshot: {
                        phase: "source",
                        source_json: evidence.source_json,
                        source_sha256: evidence.source_sha256,
                      },
                      ...(sourceOwnership
                        ? {
                            owned_conversation: {
                              provider: sourceOwnership.provider,
                              external_conversation_id:
                                sourceOwnership.external_conversation_id,
                              purpose: "measurement",
                              correlation: "create_response",
                            },
                            ...(evidence.completion === undefined
                              ? {}
                              : { completion: evidence.completion }),
                          }
                        : {}),
                    };
                    sourceReceipt = captureReceipt(await persistCapture(body));
                    sourceDigest = evidence.source_sha256;
                  } else if (evidence.phase === "extraction") {
                    if (!sourceReceipt) throw new Error("source_not_persisted");
                    if (
                      typeof evidence.source_json !== "string" ||
                      Buffer.byteLength(evidence.source_json, "utf8") >
                        750_000 ||
                      !SHA256.test(evidence.source_sha256) ||
                      createHash("sha256")
                        .update(evidence.source_json)
                        .digest("hex") !== evidence.source_sha256
                    )
                      throw new Error("extraction_capture_invalid");
                    captureReceipt(
                      await persistCapture({
                        schema_version: 1,
                        capture_ticket: input.source_capture_ticket,
                        ordinal: ++candidateOrdinal,
                        observed_at:
                          evidence.observed_at ??
                          new Date(clock()).toISOString(),
                        snapshot: {
                          phase: "extraction",
                          source_capture_id: sourceReceipt.capture_id,
                          source_json: evidence.source_json,
                          source_sha256: evidence.source_sha256,
                        },
                        ...(extractionOwnership
                          ? {
                              owned_conversation: {
                                provider: extractionOwnership.provider,
                                external_conversation_id:
                                  extractionOwnership.external_conversation_id,
                                purpose: "extraction",
                                correlation: "create_response",
                              },
                              ...(evidence.completion === undefined
                                ? {}
                                : { completion: evidence.completion }),
                            }
                          : {}),
                      }),
                    );
                  } else if (evidence.phase === "candidate") {
                    if (!sourceReceipt) throw new Error("source_not_persisted");
                    if (typeof evidence.candidate_json !== "string") return;
                    if (
                      !SHA256.test(evidence.candidate_sha256) ||
                      createHash("sha256")
                        .update(evidence.candidate_json)
                        .digest("hex") !== evidence.candidate_sha256
                    )
                      throw new Error("candidate_capture_invalid");
                    const ordinal = ++candidateOrdinal;
                    const body = {
                      schema_version: 1,
                      capture_ticket: input.source_capture_ticket,
                      ordinal,
                      observed_at: new Date(clock()).toISOString(),
                      snapshot: {
                        phase: "candidate",
                        source_capture_id: sourceReceipt.capture_id,
                        route: evidence.route,
                        candidate_json: evidence.candidate_json,
                        candidate_sha256: evidence.candidate_sha256,
                        grounding_reason: evidence.grounding_reason ?? null,
                      },
                      ...(evidence.route === "signed_in_browser" &&
                      extractionOwnership
                        ? {
                            owned_conversation: {
                              provider: extractionOwnership.provider,
                              external_conversation_id:
                                extractionOwnership.external_conversation_id,
                              purpose: "extraction",
                              correlation: "create_response",
                            },
                          }
                        : {}),
                    };
                    captureReceipt(await persistCapture(body));
                  } else {
                    throw new Error("capture_phase_invalid");
                  }
                },
              };
        const outcome = await Promise.race([
          record.adapter.execute(record.page, input.operation, input.payload, {
            proxy: record.proxy,
            expectedAccountId: record.identity.platform_account_id,
            deadlineAt,
            sourceDeadlineAt,
            signal: controller.signal,
            ...captureHooks,
          }),
          timeout,
        ]);
        if (outcome === deadline) return timedOut();
        if (
          !object(outcome) ||
          !STATUSES.has(outcome.status) ||
          !Array.isArray(outcome.evidence)
        ) {
          throw new Error("invalid_adapter_outcome");
        }
        if (
          input.source_capture_ticket !== undefined &&
          outcome.status === "completed" &&
          !sourceReceipt
        )
          return {
            status: "unknown",
            reason: "observation_capture_missing",
            evidence: [],
            ...hostReceipt,
          };
        // Adapter observations may describe a result, but cannot assert
        // whether the runner was live or which connector version was loaded.
        const {
          execution_id: _executionId,
          connector_version: _connectorVersion,
          provenance: _provenance,
          fixture: _fixture,
          ...observation
        } = outcome;
        return { ...observation, ...hostReceipt };
      } catch {
        // A timeout or crash after submit can be an external success. Rust must
        // reconcile an unknown result; this service cannot safely retry it.
        return {
          status: "unknown",
          evidence: [],
          ...hostReceipt,
        };
      } finally {
        clearTimeout(timer);
        record.busy = false;
        record.lastTouched = clock();
        entry.settledAt = clock();
      }
    })();
    entry.promise = promise;
    executions.set(input.execution_id, entry);
    return promise;
  }

  async function executeRich(sessionId, input, media, authorize) {
    if (
      !UUID.test(sessionId) ||
      !fields(input, [
        "schema_version",
        "execution_id",
        "attempt_id",
        "callback_ticket",
        "variant",
        "payload",
      ]) ||
      input.schema_version !== 1 ||
      !validId(input.execution_id) ||
      !UUID.test(input.attempt_id) ||
      typeof input.callback_ticket !== "string" ||
      !/^[0-9a-f]{2,4096}$/.test(input.callback_ticket) ||
      !object(input.variant) ||
      !object(input.payload) ||
      !Array.isArray(media)
    )
      throw new RunnerError(400, "invalid_rich_execution");
    const fingerprint = createHash("sha256")
      .update(JSON.stringify(input))
      .update(
        JSON.stringify(
          media.map(({ object_id, object_version, sha256 }) => [
            object_id,
            object_version,
            sha256,
          ]),
        ),
      )
      .digest("hex");
    const previous = executions.get(input.execution_id);
    if (previous) {
      if (previous.fingerprint !== fingerprint)
        throw new RunnerError(409, "execution_conflict");
      return previous.promise;
    }
    const record = session(sessionId);
    const hostReceipt = {
      execution_id: input.execution_id,
      provenance,
      ...(typeof record.connectorVersion === "string"
        ? { connector_version: record.connectorVersion }
        : {}),
    };
    if (!record.completed) throw new RunnerError(409, "login_required");
    if (
      !record.adapter.operations?.includes("rich_publish") ||
      typeof record.adapter.executeRich !== "function" ||
      typeof authorize !== "function"
    )
      return {
        status: "unsupported",
        reason: "rich_publication_unavailable",
        evidence: [],
        ...hostReceipt,
      };
    if (record.busy) throw new RunnerError(409, "session_busy");
    if (isChallenge(record.page))
      return { status: "challenge", evidence: [], ...hostReceipt };
    let stage;
    try {
      stage = stageRichPublication(input.payload, media, input.variant);
    } catch (error) {
      if (error instanceof RichPublicationError)
        throw new RunnerError(
          error.code === "rich_payload_too_large" ? 413 : 400,
          error.code,
        );
      throw error;
    }
    record.busy = true;
    const entry = { fingerprint, promise: null, settledAt: null };
    const promise = (async () => {
      const deadline = Symbol("execution_deadline");
      const deadlineAt = performance.now() + executionTimeoutMs;
      const controller = new AbortController();
      let timer;
      const timeout = new Promise((resolve) => {
        const expire = () => {
          const remaining = deadlineAt - performance.now();
          if (remaining > 0) {
            timer = setTimeout(expire, Math.ceil(remaining));
            return;
          }
          controller.abort();
          resolve(deadline);
        };
        timer = setTimeout(expire, executionTimeoutMs);
      });
      const unknown = (reason) => {
        if (sessions.get(sessionId) === record) sessions.delete(sessionId);
        void dispose(record).catch(() => {});
        return { status: "unknown", reason, evidence: [], ...hostReceipt };
      };
      try {
        const identity = await Promise.race([
          record.adapter.identify(record.page).catch(() => null),
          timeout,
        ]);
        if (identity === deadline) return unknown("execution_deadline");
        if (
          !validateIdentity(identity) ||
          identity.platform_account_id !== record.identity.platform_account_id
        )
          return {
            status: "login_required",
            reason: "account_identity_unverified",
            evidence: [],
            ...hostReceipt,
          };
        const outcome = await Promise.race([
          executeAuthorizedRichPublication(stage, {
            expected: {
              attempt_id: input.attempt_id,
              runner_session_id: sessionId,
              payload_hash: input.variant.payload_hash,
            },
            authorize: () =>
              authorize({
                schema_version: 1,
                callback_ticket: input.callback_ticket,
                attempt_id: input.attempt_id,
                runner_session_id: sessionId,
                payload_hash: input.variant.payload_hash,
              }),
            upload: (payload, bytes) =>
              record.adapter.executeRich(record.page, payload, bytes, {
                proxy: record.proxy,
                expectedAccountId: record.identity.platform_account_id,
                deadlineAt,
                signal: controller.signal,
              }),
          }),
          timeout,
        ]);
        if (outcome === deadline) return unknown("execution_deadline");
        if (
          !object(outcome) ||
          !STATUSES.has(outcome.status) ||
          !Array.isArray(outcome.evidence)
        )
          throw new Error("invalid_adapter_outcome");
        const {
          execution_id: _executionId,
          connector_version: _connectorVersion,
          provenance: _provenance,
          fixture: _fixture,
          ...observation
        } = outcome;
        // A fixture editor can exercise bytes and authorization, but no
        // installed adapter has rich public-structure/image readback proof.
        if (observation.status === "completed")
          return {
            status: "unknown",
            reason: "rich_public_readback_unavailable",
            evidence: [],
            ...hostReceipt,
          };
        return { ...observation, ...hostReceipt };
      } catch (error) {
        if (error instanceof RichPublicationError) {
          if (error.code === "send_authorization_denied")
            return {
              status: "unsupported",
              reason: "send_authorization_denied",
              evidence: [],
              ...hostReceipt,
            };
          return unknown(error.code);
        }
        return unknown("rich_execution_unknown");
      } finally {
        clearTimeout(timer);
        record.busy = false;
        record.lastTouched = clock();
        entry.settledAt = clock();
      }
    })();
    entry.promise = promise;
    executions.set(input.execution_id, entry);
    return promise;
  }

  async function cleanupConversation(sessionId, input, authorizeDeletion) {
    if (
      !validId(sessionId) ||
      !fields(input, [
        "execution_id",
        "expected_identity",
        "external_conversation_id",
        "action",
        "authorization_ticket",
      ]) ||
      typeof input.execution_id !== "string" ||
      !UUID.test(input.execution_id) ||
      !fields(input.expected_identity, ["provider", "platform_account_id"]) ||
      input.expected_identity.provider !== "kimi" ||
      !validId(input.expected_identity.platform_account_id) ||
      !validId(input.external_conversation_id) ||
      !["delete", "reconcile"].includes(input.action) ||
      (input.authorization_ticket !== undefined &&
        (typeof input.authorization_ticket !== "string" ||
          !/^(?:[0-9a-f]{2}){1,2048}$/.test(input.authorization_ticket)))
    )
      throw new RunnerError(400, "invalid_cleanup");
    const fingerprint = createHash("sha256")
      .update(JSON.stringify({ cleanup_session_id: sessionId, ...input }))
      .digest("hex");
    const previous = executions.get(input.execution_id);
    if (previous) {
      if (previous.fingerprint !== fingerprint)
        throw new RunnerError(409, "execution_conflict");
      return previous.promise;
    }
    const record = session(sessionId);
    const reply = (status) => ({
      execution_id: input.execution_id,
      external_conversation_id: input.external_conversation_id,
      status,
    });
    if (record.platform !== "kimi") return reply("retained");
    if (!record.completed) return reply("needs_login");
    if (record.busy) throw new RunnerError(409, "session_busy");
    if (
      input.action === "delete" &&
      (!input.authorization_ticket || typeof authorizeDeletion !== "function")
    )
      return reply("retained");
    record.busy = true;
    const entry = { fingerprint, promise: null, settledAt: null };
    const deadlineAt = performance.now() + Math.min(60_000, executionTimeoutMs);
    const expired = () => performance.now() >= deadlineAt;
    // No operation may start after the lifecycle deadline. Already-dispatched
    // requests remain ambiguous and keep the session reserved until settled.
    const page = {
      async evaluate(fn, args) {
        if (expired()) throw new Error("cleanup_deadline");
        return record.page.evaluate(fn, args);
      },
    };
    let timer;
    const work = (async () => {
      try {
        const identity = await record.adapter.identify(record.page);
        if (expired()) return reply("unknown");
        if (!validateIdentity(identity)) return reply("needs_login");
        if (
          identity.platform_account_id !==
            input.expected_identity.platform_account_id ||
          identity.platform_account_id !== record.identity.platform_account_id
        )
          return reply("retained");
        const options = {
          expectedUserId: input.expected_identity.platform_account_id,
          chatId: input.external_conversation_id,
          timeoutMs: Math.max(
            1,
            Math.min(12_000, Math.floor(deadlineAt - performance.now())),
          ),
        };
        const result =
          input.action === "reconcile"
            ? await recoverKimiConversation(page, options)
            : await deleteKimiConversation(page, {
                ...options,
                async authorizeDeletion() {
                  if (expired()) throw new Error("cleanup_deadline");
                  const authority = await authorizeDeletion({
                    schema_version: 1,
                    authorization_ticket: input.authorization_ticket,
                    runner_session_id: sessionId,
                    platform_account_id: options.expectedUserId,
                    external_conversation_id: options.chatId,
                  });
                  if (expired()) throw new Error("cleanup_deadline");
                  if (
                    typeof authority?.delete_not_after !== "string" ||
                    !Number.isFinite(Date.parse(authority.delete_not_after)) ||
                    Date.parse(authority.delete_not_after) <= Date.now()
                  )
                    throw new Error("cleanup_authorization_expired");
                  if (
                    typeof authority.retained_message_inventory_sha256 !==
                      "string" ||
                    !SHA256.test(authority.retained_message_inventory_sha256)
                  )
                    throw new Error("cleanup_inventory_required");
                  return authority;
                },
              });
        if (expired()) return reply("unknown");
        return reply(
          {
            deleted: "deleted",
            recovered: "present",
            unknown: "unknown",
            retained: "retained",
            reauth_required: "needs_login",
          }[result.status] ?? "unknown",
        );
      } catch {
        return reply("unknown");
      } finally {
        clearTimeout(timer);
        record.busy = false;
        record.lastTouched = clock();
        entry.settledAt = clock();
      }
    })();
    entry.promise = Promise.race([
      work,
      new Promise((resolve) => {
        timer = setTimeout(
          () => resolve(reply("unknown")),
          Math.max(1, Math.ceil(deadlineAt - performance.now())),
        );
      }),
    ]);
    executions.set(input.execution_id, entry);
    return entry.promise;
  }

  async function close(id) {
    const record = session(id);
    if (record.busy) throw new RunnerError(409, "session_busy");
    sessions.delete(id);
    for (const client of record.desktopClients) client.terminate();
    record.desktopClients.clear();
    await dispose(record);
  }

  async function shutdown() {
    clearInterval(maintenance);
    const records = [...sessions.values()];
    sessions.clear();
    executions.clear();
    for (const record of records) {
      for (const client of record.desktopClients) client.terminate();
      record.desktopClients.clear();
    }
    await Promise.allSettled(records.map((record) => dispose(record)));
    if (browserPromise) await (await browserPromise).close();
  }

  return {
    executionProvenance: provenance,
    create,
    desktopEndpoint,
    attachDesktopClient,
    status,
    complete,
    measurementOptions,
    execute,
    executeRich,
    cleanupConversation,
    close,
    shutdown,
    reap,
    capabilities,
  };
}
