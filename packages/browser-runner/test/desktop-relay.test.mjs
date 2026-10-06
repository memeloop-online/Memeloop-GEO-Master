import assert from "node:assert/strict";
import { once } from "node:events";
import { createServer } from "node:http";
import { after, test } from "node:test";
import WebSocket, { WebSocketServer } from "ws";
import { createRunnerServer } from "../src/server.mjs";

const upstreamHttp = createServer();
const vnc = new WebSocketServer({ server: upstreamHttp });
vnc.on("connection", (client) => {
  client.send(Buffer.from("server-hello"));
  client.on("message", (frame) => client.send(frame));
});
await new Promise((resolve) => upstreamHttp.listen(0, "127.0.0.1", resolve));
const active = new Set();
const runner = {
  desktopEndpoint(id) {
    if (id !== "fixed") throw new Error("unknown");
    return { host: "127.0.0.1", port: upstreamHttp.address().port };
  },
  attachDesktopClient(id, client) {
    const endpoint = this.desktopEndpoint(id);
    active.add(client);
    client.once("close", () => active.delete(client));
    return endpoint;
  },
};
const api = createRunnerServer({ token: "private-token", runner });
await new Promise((resolve) => api.listen(0, "127.0.0.1", resolve));
const endpoint = `ws://127.0.0.1:${api.address().port}/v1/sessions/fixed/desktop`;
after(async () => {
  for (const client of active) client.terminate();
  for (const client of vnc.clients) client.terminate();
  await new Promise((resolve) => api.close(resolve));
  await new Promise((resolve) => vnc.close(resolve));
  await new Promise((resolve) => upstreamHttp.close(resolve));
});

test("private relay requires bearer and a fixed server-owned session", async () => {
  const unauthenticated = new WebSocket(endpoint);
  const [error] = await once(unauthenticated, "error");
  assert.match(error.message, /401/);
  const wrongSession = new WebSocket(endpoint.replace("/fixed/", "/other/"), {
    headers: { authorization: "Bearer private-token" },
  });
  const [unknown] = await once(wrongSession, "error");
  assert.match(unknown.message, /404/);
});

test("private relay passes binary RFB and terminates on revocation", async () => {
  const client = new WebSocket(endpoint, {
    headers: { authorization: "Bearer private-token" },
  });
  const hello = once(client, "message");
  await once(client, "open");
  assert.equal((await hello)[0].toString(), "server-hello");
  const echo = once(client, "message");
  client.send(Buffer.from([0, 1, 2, 255]));
  assert.deepEqual((await echo)[0], Buffer.from([0, 1, 2, 255]));
  const closed = once(client, "close");
  const serverClosed = Promise.all(
    [...active].map((connection) => once(connection, "close")),
  );
  for (const connection of active) connection.terminate();
  await closed;
  await serverClosed;
  assert.equal(active.size, 0);
});
