import { createHash } from "node:crypto";
import { setTimeout as delay } from "node:timers/promises";
import { chromium } from "playwright";
import { adapters as defaultAdapters } from "./adapters.mjs";
import { createLinuxDesktopRuntime } from "./interactive-desktop.mjs";

const ID = /^[a-zA-Z0-9_-]{1,128}$/;
const VIEWPORT = Object.freeze({ width: 1280, height: 800 });
const OPERATIONS = new Set(["publish", "measure", "lookup"]);
const BROWSER_CHANNELS = new Set(["chromium", "chrome", "msedge"]);
const STATUSES = new Set([
  "unsupported",
  "login_required",
  "challenge",
  "unknown",
  "completed",
]);
const RESTORED_KIMI_IDENTITY_WAIT_MS = 4_000;

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
          operations: [...adapter.operations],
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
      if (desktop) {
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
        timeout: 30_000,
      });
      const record = {
        adapter,
        // Snapshot host configuration before executing any adapter code.
        connectorVersion: adapter.connectorVersion,
        context,
        page,
        desktop: desktopSession,
        desktopClients: new Set(),
        proxy,
        restoredKimi: input.platform === "kimi" && !!storageState,
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

  async function identityForCompletion(record) {
    // Restored Kimi storage may need the website's hydration/refresh before
    // its saved access token verifies. Wait only for a missing identity, on
    // the existing page, regardless of the runner's desktop/headless mode.
    const deadline = performance.now() + restoredKimiIdentityWaitMs;
    for (;;) {
      if (isChallenge(record.page)) throw new RunnerError(409, "challenge");
      const identity = await record.adapter.identify(record.page);
      if (validateIdentity(identity)) {
        if (
          record.identity &&
          record.identity.platform_account_id !== identity.platform_account_id
        )
          throw new RunnerError(409, "account_mismatch");
        return identity;
      }
      if (!record.restoredKimi || performance.now() >= deadline)
        throw new RunnerError(409, "login_required");
      await delay(Math.min(250, Math.max(1, deadline - performance.now())));
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
    // A restored desktop session uses the same bounded readiness path above.
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

  async function execute(input) {
    if (
      !fields(input, ["execution_id", "session_id", "operation", "payload"]) ||
      !validId(input.execution_id) ||
      !validId(input.session_id) ||
      !OPERATIONS.has(input.operation) ||
      !object(input.payload)
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
      let timer;
      const timeout = new Promise((resolve) => {
        timer = setTimeout(() => resolve(deadline), executionTimeoutMs);
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
        const outcome = await Promise.race([
          record.adapter.execute(record.page, input.operation, input.payload, {
            proxy: record.proxy,
            expectedAccountId: record.identity.platform_account_id,
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
    close,
    shutdown,
    reap,
    capabilities,
  };
}
