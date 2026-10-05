// Connect streaming JSON envelope format:
// https://connectrpc.com/docs/protocol#streaming-rpcs
// EndStreamResponse has optional metadata (header -> string[]) and error:
// https://connectrpc.com/docs/protocol#error-end-stream
const DEFAULT_MAX_FRAME_BYTES = 1_048_576;
const DEFAULT_MAX_TOTAL_BYTES = 8_388_608;
const DEFAULT_MAX_MESSAGES = 256;
const HEADER_BYTES = 5;
const decoder = new TextDecoder("utf-8", { fatal: true });

function validLimit(value) {
  return Number.isSafeInteger(value) && value > 0;
}

function object(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function parseObject(bytes) {
  let value;
  try {
    value = JSON.parse(decoder.decode(bytes));
  } catch {
    throw new Error("Connect JSON stream: invalid frame JSON");
  }
  if (!object(value))
    throw new Error("Connect JSON stream: frame must be a JSON object");
  return value;
}

function parseEnd(value) {
  if (value.metadata !== undefined) {
    if (!object(value.metadata))
      throw new Error("Connect JSON stream: invalid end metadata");
    for (const values of Object.values(value.metadata)) {
      if (
        !Array.isArray(values) ||
        values.some((item) => typeof item !== "string")
      )
        throw new Error("Connect JSON stream: invalid end metadata");
    }
  }
  // The Connect specification permits an omitted error, and the reference
  // parser also treats null as absent. Never expose server error contents.
  if (value.error != null) {
    if (!object(value.error))
      throw new Error("Connect JSON stream: invalid end error");
    throw new Error("Connect JSON stream: server error");
  }
  return { metadata: value.metadata ?? {} };
}

/**
 * Decode a single uncompressed Connect server-streaming JSON response.
 * push() accepts arbitrary byte splits; finish() is the only success path.
 * No transport object identifiers or search semantics are inferred here.
 *
 * A decoder is single-use; errors poison it. Byte budgets include headers.
 */
export function createConnectJsonDecoder({
  maxFrameBytes = DEFAULT_MAX_FRAME_BYTES,
  maxTotalBytes = DEFAULT_MAX_TOTAL_BYTES,
  maxMessages = DEFAULT_MAX_MESSAGES,
} = {}) {
  if (
    !validLimit(maxFrameBytes) ||
    !validLimit(maxTotalBytes) ||
    !validLimit(maxMessages)
  )
    throw new TypeError("Connect JSON stream: invalid limits");

  const header = new Uint8Array(HEADER_BYTES);
  const messages = [];
  let headerUsed = 0;
  let payload;
  let payloadUsed = 0;
  let flags;
  let totalBytes = 0;
  let declaredBytes = 0;
  let end;
  let closed = false;

  function fail(reason) {
    closed = true;
    throw new Error(`Connect JSON stream: ${reason}`);
  }

  function acceptFrame() {
    const value = parseObject(payload);
    if (flags === 2) {
      end = parseEnd(value);
    } else {
      messages.push(value);
    }
    headerUsed = 0;
    payload = undefined;
    payloadUsed = 0;
  }

  return {
    push(chunk) {
      if (closed) fail("decoder is closed");
      if (!(chunk instanceof Uint8Array)) fail("expected Uint8Array");
      if (end && chunk.byteLength > 0) fail("data after end");
      if (chunk.byteLength > maxTotalBytes - totalBytes)
        fail("total byte limit exceeded");
      totalBytes += chunk.byteLength;

      try {
        let offset = 0;
        while (offset < chunk.byteLength) {
          if (end) fail("data after end");
          if (headerUsed < HEADER_BYTES) {
            const count = Math.min(
              HEADER_BYTES - headerUsed,
              chunk.byteLength - offset,
            );
            header.set(chunk.subarray(offset, offset + count), headerUsed);
            headerUsed += count;
            offset += count;
            if (headerUsed < HEADER_BYTES) continue;
            flags = header[0];
            if (flags & 1) fail("compressed frames unsupported");
            if (flags !== 0 && flags !== 2) fail("invalid frame flags");
            const length = new DataView(header.buffer).getUint32(1);
            if (length > maxFrameBytes) fail("frame byte limit exceeded");
            // Account declared payloads before allocation, including when the
            // peer sends only a header and never delivers its large payload.
            if (HEADER_BYTES + length > maxTotalBytes - declaredBytes)
              fail("total byte limit exceeded");
            declaredBytes += HEADER_BYTES + length;
            if (flags === 0 && messages.length >= maxMessages)
              fail("message limit exceeded");
            payload = new Uint8Array(length);
            if (length === 0) acceptFrame();
          }
          if (!payload) continue;
          const count = Math.min(
            payload.byteLength - payloadUsed,
            chunk.byteLength - offset,
          );
          payload.set(chunk.subarray(offset, offset + count), payloadUsed);
          payloadUsed += count;
          offset += count;
          if (payloadUsed === payload.byteLength) acceptFrame();
        }
      } catch (error) {
        closed = true;
        throw error;
      }
    },
    finish() {
      if (closed) fail("decoder is closed");
      closed = true;
      if (headerUsed || payload) fail("truncated frame");
      if (!end) fail("missing end frame");
      return { messages, end };
    },
  };
}
