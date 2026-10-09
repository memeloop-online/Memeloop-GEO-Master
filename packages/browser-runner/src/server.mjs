import { createServer } from "node:http";
import { timingSafeEqual } from "node:crypto";
import { pathToFileURL } from "node:url";
import WebSocket, { WebSocketServer } from "ws";
import { createRunner, RunnerError } from "./runner.mjs";
import { readRichMultipart } from "./rich-transport.mjs";

const CALLBACK_PATH = "/internal/v1/publication-send/authorize";
const CAPTURE_PATH = "/internal/v1/observation-captures";
const CLEANUP_PATH = "/internal/v1/provider-conversation-cleanup/authorize";

function serviceCallback(
  origin,
  token,
  path,
  maxResponseBytes,
  timeoutMs = 10_000,
) {
  // Deployment configuration only, never a model/request-selected URL.
  // Private cluster service traffic may use HTTP; external deployments use TLS.
  if (!origin && !token) return null;
  if (typeof origin !== "string" || typeof token !== "string")
    throw new Error("rich_callback_config_incomplete");
  let url;
  try {
    url = new URL(origin);
  } catch {
    throw new Error("invalid_rich_callback_origin");
  }
  if (
    origin !== url.origin ||
    !["https:", "http:"].includes(url.protocol) ||
    token.length < 32 ||
    token.length > 512 ||
    !/^[\x21-\x7e]+$/.test(token)
  )
    throw new Error("invalid_rich_callback_config");
  return async (body, { signal } = {}) => {
    const response = await fetch(`${origin}${path}`, {
      method: "POST",
      headers: {
        authorization: `Bearer ${token}`,
        "content-type": "application/json",
      },
      body: JSON.stringify(body),
      redirect: "error",
      signal: signal
        ? AbortSignal.any([signal, AbortSignal.timeout(timeoutMs)])
        : AbortSignal.timeout(timeoutMs),
    });
    if (response.status !== 200)
      throw new Error("service_callback_unavailable");
    const length = Number(response.headers.get("content-length") ?? 0);
    if (length > maxResponseBytes)
      throw new Error("service_response_oversized");
    const reader = response.body?.getReader();
    if (!reader) throw new Error("send_authorization_missing");
    const chunks = [];
    let size = 0;
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > maxResponseBytes) {
        await reader.cancel();
        throw new Error("service_response_oversized");
      }
      chunks.push(value);
    }
    return JSON.parse(Buffer.concat(chunks, size).toString("utf8"));
  };
}

function authorized(header, token) {
  if (typeof header !== "string" || !header.startsWith("Bearer ")) return false;
  const received = Buffer.from(header.slice(7));
  const expected = Buffer.from(token);
  return (
    received.length === expected.length && timingSafeEqual(received, expected)
  );
}

async function readJson(request) {
  const chunks = [];
  let size = 0;
  for await (const chunk of request) {
    size += chunk.length;
    if (size > 1024 * 1024) throw new RunnerError(413, "payload_too_large");
    chunks.push(chunk);
  }
  try {
    return JSON.parse(Buffer.concat(chunks).toString("utf8"));
  } catch {
    throw new RunnerError(400, "invalid_json");
  }
}

function send(response, status, value) {
  response.writeHead(status, {
    "content-type": "application/json; charset=utf-8",
    "cache-control": "no-store",
    "x-content-type-options": "nosniff",
  });
  response.end(JSON.stringify(value));
}

export function createRunnerServer({
  token = process.env.GEO_BROWSER_RUNNER_TOKEN,
  runner = createRunner(),
  callbackOrigin = process.env.GEO_BROWSER_RUNNER_CALLBACK_ORIGIN,
  callbackToken = process.env.GEO_BROWSER_RUNNER_CALLBACK_TOKEN,
  captureOrigin = process.env.GEO_BROWSER_RUNNER_CAPTURE_ORIGIN ??
    callbackOrigin,
  captureToken = process.env.GEO_BROWSER_RUNNER_CAPTURE_TOKEN ?? callbackToken,
} = {}) {
  if (!token || typeof token !== "string")
    throw new Error("GEO_BROWSER_RUNNER_TOKEN_required");
  const authorizeRich = serviceCallback(
    callbackOrigin,
    callbackToken,
    CALLBACK_PATH,
    8192,
  );
  const persistCapture = serviceCallback(
    captureOrigin,
    captureToken,
    CAPTURE_PATH,
    8192,
  );
  const observationAi = {
    policy: serviceCallback(
      captureOrigin,
      captureToken,
      "/internal/v1/observation-ai/policy",
      8192,
    ),
    extract: serviceCallback(
      captureOrigin,
      captureToken,
      "/internal/v1/observation-ai/extract",
      320_000,
      90_000,
    ),
  };
  const authorizeCleanup = serviceCallback(
    callbackOrigin,
    callbackToken,
    CLEANUP_PATH,
    8192,
  );
  const server = createServer(async (request, response) => {
    try {
      if (!authorized(request.headers.authorization, token)) {
        send(response, 401, { error: "unauthorized" });
        return;
      }
      const path = new URL(request.url, "http://localhost").pathname;
      const parts = path.split("/").filter(Boolean);
      if (request.method === "GET" && path === "/v1/capabilities") {
        send(response, 200, runner.capabilities());
      } else if (request.method === "POST" && path === "/v1/sessions") {
        send(response, 201, await runner.create(await readJson(request)));
      } else if (request.method === "POST" && path === "/v1/executions") {
        send(
          response,
          200,
          await runner.execute(
            await readJson(request),
            persistCapture,
            observationAi,
          ),
        );
      } else if (
        parts.length === 4 &&
        parts[0] === "v1" &&
        parts[1] === "sessions" &&
        parts[3] === "cleanup-conversation" &&
        request.method === "POST"
      ) {
        send(
          response,
          200,
          await runner.cleanupConversation(
            parts[2],
            await readJson(request),
            authorizeCleanup,
          ),
        );
      } else if (
        parts.length === 4 &&
        parts[0] === "v1" &&
        parts[1] === "sessions" &&
        parts[3] === "execute-rich" &&
        request.method === "POST"
      ) {
        if (!authorizeRich) {
          send(response, 503, { error: "rich_callback_unavailable" });
          return;
        }
        const input = await readRichMultipart(request);
        try {
          send(
            response,
            200,
            await runner.executeRich(
              parts[2],
              input.metadata,
              input.media,
              authorizeRich,
            ),
          );
        } finally {
          input.dispose();
        }
      } else if (
        parts.length === 4 &&
        parts[0] === "v1" &&
        parts[1] === "sessions" &&
        parts[3] === "status" &&
        request.method === "GET"
      ) {
        send(response, 200, await runner.status(parts[2]));
      } else if (
        parts.length === 4 &&
        parts[0] === "v1" &&
        parts[1] === "sessions" &&
        parts[3] === "measurement-options" &&
        request.method === "GET"
      ) {
        send(response, 200, await runner.measurementOptions(parts[2]));
      } else if (
        parts.length === 4 &&
        parts[0] === "v1" &&
        parts[1] === "sessions" &&
        parts[3] === "complete" &&
        request.method === "POST"
      ) {
        send(response, 200, await runner.complete(parts[2]));
      } else if (
        parts.length === 3 &&
        parts[0] === "v1" &&
        parts[1] === "sessions" &&
        request.method === "DELETE"
      ) {
        await runner.close(parts[2]);
        send(response, 200, { closed: true });
      } else {
        send(response, 404, { error: "not_found" });
      }
    } catch (error) {
      // Never serialize a browser/Playwright error; it may include URLs, typed
      // credentials, cookies, storage state, or request payloads.
      send(response, error instanceof RunnerError ? error.status : 503, {
        error: error instanceof RunnerError ? error.code : "runner_unavailable",
      });
    }
  });
  const upgrades = new WebSocketServer({
    noServer: true,
    maxPayload: 8 * 1024 * 1024,
  });
  server.on("upgrade", (request, socket, head) => {
    const path = new URL(request.url, "http://localhost").pathname;
    const match = /^\/v1\/sessions\/([a-zA-Z0-9_-]{1,128})\/desktop$/.exec(
      path,
    );
    if (!match || !authorized(request.headers.authorization, token)) {
      socket.write("HTTP/1.1 401 Unauthorized\r\nConnection: close\r\n\r\n");
      socket.destroy();
      return;
    }
    let endpoint;
    try {
      endpoint = runner.desktopEndpoint(match[1]);
    } catch {
      socket.write("HTTP/1.1 404 Not Found\r\nConnection: close\r\n\r\n");
      socket.destroy();
      return;
    }
    upgrades.handleUpgrade(request, socket, head, (client) => {
      try {
        runner.attachDesktopClient(match[1], client);
      } catch {
        client.terminate();
        return;
      }
      const upstream = new WebSocket(`ws://127.0.0.1:${endpoint.port}/`, {
        maxPayload: 8 * 1024 * 1024,
      });
      // Fail closed on either end closing; no password or target in client frames.
      const close = () => {
        client.terminate();
        upstream.terminate();
      };
      client.on("message", (payload, isBinary) => {
        if (upstream.readyState === WebSocket.OPEN)
          upstream.send(payload, { binary: isBinary }, (error) => {
            if (error) close();
          });
      });
      upstream.on("message", (payload, isBinary) => {
        if (client.readyState === WebSocket.OPEN)
          client.send(payload, { binary: isBinary }, (error) => {
            if (error) close();
          });
      });
      client.on("close", close);
      upstream.on("close", close);
      client.on("error", close);
      upstream.on("error", close);
    });
  });
  return server;
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  const server = createRunnerServer();
  const port = Number(process.env.GEO_BROWSER_RUNNER_PORT ?? 38080);
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error("invalid_runner_port");
  }
  // Explicit non-loopback deployment requires network policy/TLS at ingress.
  server.listen(port, process.env.GEO_BROWSER_RUNNER_HOST ?? "127.0.0.1");
}
