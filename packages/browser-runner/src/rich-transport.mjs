import { createHash } from "node:crypto";
import Busboy from "@fastify/busboy";
import { RunnerError } from "./runner.mjs";

const MAX_METADATA_BYTES = 16 * 1024 * 1024;
const MAX_IMAGE_BYTES = 100 * 1024 * 1024;
const MAX_TOTAL_BYTES = 512 * 1024 * 1024;
const MEDIA_FIELD =
  /^media_([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})_([1-9][0-9]*)$/;
const SHA256 = /^[0-9a-f]{64}$/;

function invalid() {
  return new RunnerError(400, "invalid_rich_transport");
}

function key(id, version) {
  return `${id.toLowerCase()}:${version}`;
}

export async function readRichMultipart(request) {
  if (
    typeof request.headers["content-type"] !== "string" ||
    !/^multipart\/form-data\s*;/i.test(request.headers["content-type"])
  )
    throw new RunnerError(415, "multipart_required");
  return new Promise((resolve, reject) => {
    let parser;
    try {
      parser = new Busboy({
        headers: request.headers,
        isPartAFile: (name) => name !== "metadata",
        limits: {
          fields: 1,
          files: 256,
          parts: 257,
          fieldSize: MAX_METADATA_BYTES,
          fileSize: MAX_IMAGE_BYTES,
          fieldNameSize: 160,
          headerPairs: 12,
        },
      });
    } catch {
      reject(invalid());
      return;
    }
    let settled = false;
    const deadline = setTimeout(
      () => fail(new RunnerError(408, "rich_upload_deadline")),
      120_000,
    );
    let metadata;
    let total = 0;
    const parts = new Map();
    const streams = [];
    const pendingChunks = new Set();
    const fail = (error = invalid()) => {
      if (settled) return;
      settled = true;
      clearTimeout(deadline);
      request.unpipe(parser);
      parser.destroy();
      for (const part of parts.values()) part.bytes.fill(0);
      for (const chunk of pendingChunks) chunk.fill(0);
      for (const stream of streams) stream.destroy();
      // Drain no further body; close the incoming connection after responding.
      request.resume();
      reject(error);
    };
    parser.on(
      "field",
      (name, value, nameTruncated, valueTruncated, encoding, mime) => {
        if (
          name !== "metadata" ||
          metadata !== undefined ||
          nameTruncated ||
          valueTruncated ||
          mime !== "application/json" ||
          !["7bit", "binary", "8bit"].includes(encoding)
        ) {
          fail();
          return;
        }
        try {
          metadata = JSON.parse(value);
        } catch {
          fail();
        }
      },
    );
    parser.on("file", (name, stream, filename, _encoding, mime) => {
      streams.push(stream);
      const match = MEDIA_FIELD.exec(name);
      const version = match ? Number(match[2]) : NaN;
      const mediaKey = match && key(match[1], version);
      if (
        !match ||
        !Number.isSafeInteger(version) ||
        filename !== undefined ||
        !["image/png", "image/jpeg", "image/webp"].includes(mime) ||
        parts.has(mediaKey)
      ) {
        stream.resume();
        fail();
        return;
      }
      const chunks = [];
      const hash = createHash("sha256");
      let size = 0;
      stream.on("limit", () =>
        fail(new RunnerError(413, "rich_payload_too_large")),
      );
      stream.on("data", (chunk) => {
        pendingChunks.add(chunk);
        size += chunk.length;
        total += chunk.length;
        if (size > MAX_IMAGE_BYTES || total > MAX_TOTAL_BYTES) {
          fail(new RunnerError(413, "rich_payload_too_large"));
          return;
        }
        hash.update(chunk);
        chunks.push(chunk);
      });
      stream.on("end", () => {
        if (settled) return;
        const bytes = Buffer.concat(chunks, size);
        for (const chunk of chunks) {
          chunk.fill(0);
          pendingChunks.delete(chunk);
        }
        parts.set(mediaKey, {
          object_id: match[1],
          object_version: version,
          media_type: mime,
          sha256: hash.digest("hex"),
          bytes,
        });
      });
      stream.on("error", () => fail());
    });
    parser.on("partsLimit", () =>
      fail(new RunnerError(413, "rich_payload_too_large")),
    );
    parser.on("filesLimit", () =>
      fail(new RunnerError(413, "rich_payload_too_large")),
    );
    parser.on("fieldsLimit", () =>
      fail(new RunnerError(413, "rich_payload_too_large")),
    );
    parser.on("error", () => fail());
    request.on("aborted", () => fail());
    request.on("error", () => fail());
    parser.on("finish", () => {
      if (settled) return;
      if (
        !metadata ||
        typeof metadata !== "object" ||
        Array.isArray(metadata) ||
        !Array.isArray(metadata.payload?.media)
      ) {
        fail();
        return;
      }
      const expected = new Map();
      for (const item of metadata.payload.media) {
        const object = item?.object;
        const mediaKey = object && key(object.object_id, object.object_version);
        if (
          typeof object?.object_id !== "string" ||
          !Number.isSafeInteger(object.object_version) ||
          typeof object.sha256 !== "string" ||
          !SHA256.test(object.sha256) ||
          !["image/png", "image/jpeg", "image/webp"].includes(item.media_type)
        ) {
          fail();
          return;
        }
        expected.set(mediaKey, item);
      }
      if (parts.size !== expected.size) {
        fail();
        return;
      }
      for (const [mediaKey, item] of expected) {
        const part = parts.get(mediaKey);
        if (
          !part ||
          part.sha256 !== item.object.sha256 ||
          part.bytes.length !== item.byte_len ||
          part.media_type !== item.media_type
        ) {
          fail(new RunnerError(400, "media_parts_mismatch"));
          return;
        }
      }
      settled = true;
      clearTimeout(deadline);
      resolve({
        metadata,
        media: [...parts.values()],
        dispose() {
          for (const part of parts.values()) part.bytes.fill(0);
        },
      });
    });
    request.pipe(parser);
  });
}
