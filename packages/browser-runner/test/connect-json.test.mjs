import assert from "node:assert/strict";
import { test } from "node:test";
import { createConnectJsonDecoder } from "../src/connect-json.mjs";

const encoder = new TextEncoder();

function frame(flags, body) {
  const payload = typeof body === "string" ? encoder.encode(body) : body;
  const result = new Uint8Array(payload.length + 5);
  result[0] = flags;
  new DataView(result.buffer).setUint32(1, payload.length);
  result.set(payload, 5);
  return result;
}

function join(...chunks) {
  const result = new Uint8Array(
    chunks.reduce((size, chunk) => size + chunk.byteLength, 0),
  );
  let offset = 0;
  for (const chunk of chunks) {
    result.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return result;
}

const message = frame(0, '{"text":"雪🌨️"}');
const end = frame(2, "{}");

test("decodes byte-by-byte splits across headers, UTF-8 and frames", () => {
  const data = join(message, frame(0, '{"delta":1}'), end);
  const decoder = createConnectJsonDecoder();
  for (const byte of data) decoder.push(Uint8Array.of(byte));
  assert.deepEqual(decoder.finish(), {
    messages: [{ text: "雪🌨️" }, { delta: 1 }],
    end: { metadata: {} },
  });
  assert.throws(() => decoder.finish(), /decoder is closed/);
});

test("accepts omitted, empty, and populated end metadata plus null error", () => {
  for (const value of [
    {},
    { metadata: {} },
    { metadata: { "x-correlation": ["one", "two"], unused: [] } },
    { error: null },
  ]) {
    const decoder = createConnectJsonDecoder();
    decoder.push(frame(2, JSON.stringify(value)));
    assert.deepEqual(decoder.finish().end, {
      metadata: value.metadata ?? {},
    });
  }
});

test("requires a valid end and forbids trailing data even in the same chunk", () => {
  for (const data of [
    new Uint8Array(),
    message,
    Uint8Array.of(0),
    frame(0, '{"part":1}').subarray(0, 7),
    frame(2, "{}").subarray(0, 6),
  ]) {
    const decoder = createConnectJsonDecoder();
    decoder.push(data);
    assert.throws(() => decoder.finish(), /missing end frame|truncated frame/);
  }
  for (const data of [
    join(end, message),
    join(end, end),
    join(end, Uint8Array.of(0)),
  ]) {
    const decoder = createConnectJsonDecoder();
    assert.throws(() => decoder.push(data), /data after end/);
  }
  const decoder = createConnectJsonDecoder();
  decoder.push(end);
  assert.throws(() => decoder.push(message), /data after end/);
});

test("rejects compression bits, reserved flags, and malformed UTF-8 or JSON", () => {
  const cases = [
    frame(1, "{}"),
    frame(3, "{}"),
    frame(4, "{}"),
    frame(0, new Uint8Array()),
    frame(0, Uint8Array.of(0xff)),
    frame(0, '{"text":'),
    frame(0, "null"),
    frame(0, "[1]"),
    frame(2, "[]"),
  ];
  for (const data of cases) {
    const decoder = createConnectJsonDecoder();
    assert.throws(() => decoder.push(data), /Connect JSON stream:/);
    assert.throws(() => decoder.finish(), /decoder is closed/);
  }
});

test("rejects non-null server errors without leaking server data", () => {
  for (const error of [
    { code: "permission_denied", message: "PRIVATE_SAMPLE_DO_NOT_EXPOSE" },
    {},
    "malformed",
  ]) {
    const decoder = createConnectJsonDecoder();
    const data = frame(2, JSON.stringify({ error }));
    assert.throws(
      () => decoder.push(data),
      (failure) =>
        failure instanceof Error &&
        failure.message.startsWith("Connect JSON stream:") &&
        !failure.message.includes("PRIVATE_SAMPLE_DO_NOT_EXPOSE"),
    );
    assert.throws(() => decoder.finish(), /decoder is closed/);
  }
});

test("validates metadata as a map of string arrays", () => {
  for (const metadata of [null, [], "value", { key: "one" }, { key: [1] }]) {
    const decoder = createConnectJsonDecoder();
    assert.throws(
      () => decoder.push(frame(2, JSON.stringify({ metadata }))),
      /invalid end metadata/,
    );
  }
});

test("enforces byte and message budgets before accepting data", () => {
  assert.throws(
    () => createConnectJsonDecoder({ maxTotalBytes: -1 }),
    /invalid limits/,
  );
  assert.throws(
    () => createConnectJsonDecoder({ maxFrameBytes: 1.5 }),
    /invalid limits/,
  );
  const frameLimited = createConnectJsonDecoder({ maxFrameBytes: 2 });
  assert.throws(() => frameLimited.push(message.subarray(0, 5)), /frame byte/);

  const totalLimited = createConnectJsonDecoder({
    maxTotalBytes: message.length + end.length - 1,
  });
  totalLimited.push(message);
  assert.throws(() => totalLimited.push(end), /total byte/);
  const declaredLimited = createConnectJsonDecoder({ maxTotalBytes: 16 });
  assert.throws(
    () => declaredLimited.push(message.subarray(0, 5)),
    /total byte/,
  );

  const messageLimited = createConnectJsonDecoder({ maxMessages: 1 });
  messageLimited.push(message);
  assert.throws(
    () => messageLimited.push(frame(0, "{}").subarray(0, 5)),
    /message limit/,
  );
  const endOnly = createConnectJsonDecoder({ maxMessages: 1 });
  endOnly.push(end);
  assert.deepEqual(endOnly.finish().messages, []);
});

test("does not include invalid user JSON in thrown errors", () => {
  const decoder = createConnectJsonDecoder();
  assert.throws(
    () => decoder.push(frame(0, '{"secret":"PRIVATE_SAMPLE_DO_NOT_EXPOSE"')),
    (error) => !error.message.includes("PRIVATE_SAMPLE_DO_NOT_EXPOSE"),
  );
});
