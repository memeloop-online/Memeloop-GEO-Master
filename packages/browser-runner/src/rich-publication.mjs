import { createHash } from "node:crypto";

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const SHA256 = /^[0-9a-f]{64}$/;
const MEDIA_TYPES = new Set(["image/png", "image/jpeg", "image/webp"]);
const ROOT_NODES = new Set([
  "paragraph",
  "heading",
  "bulletList",
  "orderedList",
  "codeBlock",
  "table",
  "media",
]);
const CHILDREN = {
  paragraph: new Set(["text", "hardBreak"]),
  heading: new Set(["text", "hardBreak"]),
  bulletList: new Set(["listItem"]),
  orderedList: new Set(["listItem"]),
  listItem: new Set(["paragraph", "bulletList", "orderedList"]),
  codeBlock: new Set(["text"]),
  table: new Set(["tableRow"]),
  tableRow: new Set(["tableHeader", "tableCell"]),
  tableHeader: new Set(["paragraph"]),
  tableCell: new Set(["paragraph"]),
};
const MAX_IMAGE_BYTES = 100 * 1024 * 1024;
const MAX_TOTAL_BYTES = 512 * 1024 * 1024;
const MAX_UNIQUE_IMAGES = 256;
const MAX_NODES = 10_000;
const MAX_TEXT_BYTES = 1_000_000;
const stagedBytes = new WeakMap();

export class RichPublicationError extends Error {
  constructor(code) {
    super(code);
    this.name = "RichPublicationError";
    this.code = code;
  }
}

function fail(code) {
  throw new RichPublicationError(code);
}

function record(value, keys, required = keys) {
  if (
    value === null ||
    typeof value !== "object" ||
    Array.isArray(value) ||
    Object.getPrototypeOf(value) !== Object.prototype ||
    Object.keys(value).some((key) => !keys.includes(key)) ||
    required.some((key) => !Object.hasOwn(value, key))
  )
    fail("invalid_rich_payload");
}

function uuid(value) {
  return (
    typeof value === "string" &&
    UUID.test(value) &&
    value.toLowerCase() !== "00000000-0000-0000-0000-000000000000"
  );
}

function positive(value, limit) {
  return Number.isSafeInteger(value) && value >= 1 && value <= limit;
}

function text(value, limits) {
  if (typeof value !== "string") fail("invalid_rich_payload");
  limits.bytes += Buffer.byteLength(value, "utf8");
  if (limits.bytes > MAX_TEXT_BYTES) fail("rich_payload_too_large");
}

function objectKey(value) {
  record(value, ["object_id", "object_version", "sha256"]);
  if (
    !uuid(value.object_id) ||
    !positive(value.object_version, Number.MAX_SAFE_INTEGER) ||
    typeof value.sha256 !== "string" ||
    !SHA256.test(value.sha256)
  )
    fail("invalid_media_identity");
  return `${value.object_id.toLowerCase()}:${value.object_version}`;
}

function mediaReference(attrs, limits) {
  record(attrs, ["object_id", "object_version", "sha256", "alt", "caption"]);
  objectKey({
    object_id: attrs.object_id,
    object_version: attrs.object_version,
    sha256: attrs.sha256,
  });
  text(attrs.alt, limits);
  text(attrs.caption, limits);
  if (!attrs.alt.trim()) fail("invalid_rich_payload");
}

function mark(value, limits) {
  record(value, ["type", "attrs"], ["type"]);
  if (value.type === "link") {
    record(value, ["type", "attrs"]);
    record(value.attrs, ["href", "title"], ["href"]);
    const { href, title } = value.attrs;
    if (
      typeof href !== "string" ||
      !href ||
      href.length > 2048 ||
      /[\s\\\u0000-\u001f\u007f]/u.test(href) ||
      href.startsWith("//")
    )
      fail("invalid_rich_link");
    try {
      const parsed = new URL(href, "https://content.invalid/");
      if (
        !["https:", "http:", "mailto:"].includes(parsed.protocol) ||
        (parsed.protocol === "mailto:" && !parsed.pathname) ||
        (parsed.protocol !== "mailto:" &&
          (parsed.username || parsed.password || !parsed.hostname))
      )
        fail("invalid_rich_link");
    } catch {
      fail("invalid_rich_link");
    }
    if (title !== undefined) {
      text(title, limits);
      if (title.length > 1024 || /[\u0000-\u001f\u007f]/u.test(title))
        fail("invalid_rich_link");
    }
  } else if (
    !["bold", "italic", "strike", "underline", "code"].includes(value.type) ||
    Object.hasOwn(value, "attrs")
  ) {
    fail("unsupported_rich_node");
  }
}

function cellAttrs(attrs) {
  if (attrs === undefined || attrs === null) return;
  record(attrs, ["colspan", "rowspan", "colwidth", "textAlign"], []);
  if (
    (attrs.colspan !== undefined && !positive(attrs.colspan, 32)) ||
    (attrs.rowspan !== undefined && !positive(attrs.rowspan, 100)) ||
    (attrs.textAlign !== undefined &&
      !["left", "center", "right"].includes(attrs.textAlign)) ||
    (attrs.colwidth !== undefined &&
      (!Array.isArray(attrs.colwidth) ||
        attrs.colwidth.length !== (attrs.colspan ?? 1) ||
        attrs.colwidth.some((width) => !positive(width, 65535))))
  )
    fail("invalid_rich_payload");
}

function tableShape(rows) {
  if (rows.length > 100) fail("rich_payload_too_large");
  const occupied = Array(32).fill(0);
  let width;
  rows.forEach((row, rowIndex) => {
    let column = 0;
    for (const cell of row.content) {
      while (column < 32 && occupied[column] > rowIndex) column++;
      const span = cell.attrs?.colspan ?? 1;
      const height = cell.attrs?.rowspan ?? 1;
      if (column + span > 32 || rowIndex + height > rows.length)
        fail("invalid_rich_payload");
      for (let i = column; i < column + span; i++) {
        if (occupied[i] > rowIndex) fail("invalid_rich_payload");
        occupied[i] = rowIndex + height;
      }
      column += span;
    }
    const rowWidth = occupied.findLastIndex((value) => value > rowIndex) + 1;
    if (rowWidth === 0 || (width !== undefined && width !== rowWidth))
      fail("invalid_rich_payload");
    width = rowWidth;
    if (occupied.slice(0, width).some((value) => value <= rowIndex))
      fail("invalid_rich_payload");
  });
}

function node(value, parent, depth, limits, occurrences) {
  if (++limits.nodes > MAX_NODES || depth > 24) fail("rich_payload_too_large");
  record(value, ["type", "attrs", "content", "text", "marks"], ["type"]);
  const { type } = value;
  if (
    typeof type !== "string" ||
    (!ROOT_NODES.has(type) &&
      ![
        "text",
        "hardBreak",
        "listItem",
        "tableRow",
        "tableHeader",
        "tableCell",
      ].includes(type))
  )
    fail("unsupported_rich_node");
  if (parent ? !CHILDREN[parent]?.has(type) : !ROOT_NODES.has(type))
    fail("unsupported_rich_node");
  if (type === "text") {
    record(value, ["type", "text", "marks"], ["type", "text"]);
    text(value.text, limits);
    if (
      !value.text ||
      (value.marks !== undefined && !Array.isArray(value.marks))
    )
      fail("invalid_rich_payload");
    const seen = new Set();
    for (const current of value.marks ?? []) {
      mark(current, limits);
      if (seen.has(current.type)) fail("invalid_rich_payload");
      seen.add(current.type);
    }
    if (seen.has("code") && seen.size !== 1) fail("invalid_rich_payload");
    if (parent === "codeBlock" && seen.size) fail("unsupported_rich_node");
    return;
  }
  if (type === "hardBreak") {
    record(value, ["type"]);
    return;
  }
  if (type === "media") {
    record(value, ["type", "attrs"]);
    mediaReference(value.attrs, limits);
    occurrences.push(value.attrs);
    return;
  }
  const attrsRequired = ["heading", "orderedList"].includes(type);
  const attrsOptional = ["codeBlock", "tableHeader", "tableCell"].includes(
    type,
  );
  record(
    value,
    attrsRequired || attrsOptional
      ? ["type", "attrs", "content"]
      : ["type", "content"],
    attrsRequired
      ? ["type", "attrs", "content"]
      : type === "paragraph"
        ? ["type"]
        : ["type", "content"],
  );
  // Rust omits an empty paragraph's content. Keep the frozen wire payload
  // unchanged so validation does not change its canonical publication hash.
  const content =
    type === "paragraph" && value.content === undefined ? [] : value.content;
  if (!Array.isArray(content) || content.length > MAX_NODES)
    fail("invalid_rich_payload");
  if (attrsRequired) {
    if (type === "heading") {
      record(value.attrs, ["level"]);
      if (!positive(value.attrs.level, 6)) fail("invalid_rich_payload");
    } else {
      record(value.attrs, ["start"]);
      if (!positive(value.attrs.start, 0xffffffff))
        fail("invalid_rich_payload");
    }
  } else if (
    type === "codeBlock" &&
    value.attrs !== undefined &&
    value.attrs !== null
  ) {
    record(value.attrs, ["language"]);
    if (
      value.attrs.language !== null &&
      (typeof value.attrs.language !== "string" ||
        !/^[a-zA-Z0-9_+#-]{0,64}$/.test(value.attrs.language))
    )
      fail("invalid_rich_payload");
  } else if (type === "tableCell" || type === "tableHeader") {
    cellAttrs(value.attrs);
  }
  if (!["paragraph", "codeBlock"].includes(type) && !content.length)
    fail("invalid_rich_payload");
  if (type === "listItem" && content[0]?.type !== "paragraph")
    fail("invalid_rich_payload");
  for (const child of content)
    node(child, type, depth + 1, limits, occurrences);
  if (type === "table") tableShape(content);
}

export function validateRichPublicationPayload(payload) {
  record(payload, [
    "schema_version",
    "format",
    "content_revision_id",
    "policy_version",
    "document",
    "media",
  ]);
  if (
    payload.schema_version !== 2 ||
    payload.format !== "rich_markdown.v2" ||
    payload.policy_version !== "deterministic-rich-markdown-v2" ||
    !uuid(payload.content_revision_id)
  )
    fail("unsupported_rich_format");
  record(payload.document, ["title", "blocks", "schema_version"]);
  const { document, media } = payload;
  if (
    document.schema_version !== 2 ||
    typeof document.title !== "string" ||
    !document.title.trim() ||
    Buffer.byteLength(document.title, "utf8") > 4096 ||
    !Array.isArray(document.blocks) ||
    !document.blocks.length ||
    document.blocks.length > MAX_NODES ||
    !Array.isArray(media) ||
    media.length > MAX_NODES
  )
    fail("invalid_rich_payload");
  const limits = { nodes: 0, bytes: 0 };
  text(document.title, limits);
  const occurrences = [];
  const blockIds = new Set();
  for (const block of document.blocks) {
    record(
      block,
      ["block_id", "kind", "text", "citation_ids", "items", "rich"],
      ["block_id", "kind", "text"],
    );
    if (
      typeof block.block_id !== "string" ||
      !UUID.test(block.block_id) ||
      blockIds.has(block.block_id.toLowerCase())
    )
      fail("invalid_rich_payload");
    blockIds.add(block.block_id.toLowerCase());
    if (++limits.nodes > MAX_NODES) fail("rich_payload_too_large");
    text(block.text, limits);
    if (
      !Array.isArray(block.citation_ids ?? []) ||
      block.citation_ids?.some((id) => !uuid(id)) ||
      !Array.isArray(block.items ?? []) ||
      block.items?.some((item) => typeof item !== "string")
    )
      fail("invalid_rich_payload");
    for (const item of block.items ?? []) text(item, limits);
    if (block.kind === "rich") {
      if (block.text || block.items?.length) fail("invalid_rich_payload");
      record(block.rich, ["version", "node"]);
      if (block.rich.version !== 1) fail("unsupported_rich_format");
      node(block.rich.node, null, 0, limits, occurrences);
    } else if (
      !["heading", "paragraph", "list"].includes(block.kind) ||
      Object.hasOwn(block, "rich") ||
      (block.kind !== "list" && block.items?.length) ||
      (block.kind === "list" && !block.items?.length) ||
      (!block.text.trim() && !block.items?.length)
    ) {
      fail("invalid_rich_payload");
    }
  }
  if (media.length !== occurrences.length) fail("media_manifest_mismatch");
  const distinct = new Map();
  media.forEach((item, index) => {
    record(item, [
      "binding_id",
      "object",
      "media_type",
      "byte_len",
      "width",
      "height",
      "alt",
      "caption",
      "role",
    ]);
    const reference = occurrences[index];
    const key = objectKey(item.object);
    if (
      !uuid(item.binding_id) ||
      item.role !== "image" ||
      !MEDIA_TYPES.has(item.media_type) ||
      !positive(item.byte_len, MAX_IMAGE_BYTES) ||
      !positive(item.width, 16384) ||
      !positive(item.height, 16384) ||
      item.width * item.height > 100_000_000 ||
      item.object.object_id.toLowerCase() !==
        reference.object_id.toLowerCase() ||
      item.object.object_version !== reference.object_version ||
      item.object.sha256 !== reference.sha256 ||
      item.alt !== reference.alt ||
      item.caption !== reference.caption
    )
      fail("media_manifest_mismatch");
    const previous = distinct.get(key);
    if (previous) {
      if (
        previous.object.sha256 !== item.object.sha256 ||
        previous.binding_id.toLowerCase() !== item.binding_id.toLowerCase() ||
        previous.media_type !== item.media_type ||
        previous.byte_len !== item.byte_len ||
        previous.width !== item.width ||
        previous.height !== item.height
      )
        fail("media_identity_conflict");
    } else {
      distinct.set(key, item);
    }
  });
  if (distinct.size > MAX_UNIQUE_IMAGES) fail("rich_payload_too_large");
  let total = 0;
  for (const item of distinct.values()) total += item.byte_len;
  if (total > MAX_TOTAL_BYTES) fail("rich_payload_too_large");
  return distinct;
}

function freeze(value) {
  if (value && typeof value === "object") {
    for (const child of Object.values(value)) freeze(child);
    Object.freeze(value);
  }
  return value;
}

function canonicalJson(value) {
  if (Array.isArray(value))
    return `[${value.map((element) => canonicalJson(element)).join(",")}]`;
  if (value && typeof value === "object")
    return `{${Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`)
      .join(",")}}`;
  return JSON.stringify(value);
}

function variantHash(title, markdown, payload) {
  const hash = createHash("sha256");
  for (const part of [
    "rich-publication-payload-v2",
    title,
    markdown,
    canonicalJson(payload),
  ]) {
    const value = Buffer.from(part, "utf8");
    const length = Buffer.alloc(8);
    length.writeBigUInt64BE(BigInt(value.byteLength));
    hash.update(length).update(value);
  }
  return hash.digest("hex");
}

/**
 * Build a private, one-execution snapshot. `mediaParts` is exactly one raw
 * byte part per unique object version; repeated document occurrences remain
 * distinct in the frozen manifest. The owning API must have verified image
 * decoding and project-scoped media authorization before supplying the parts.
 * `variant` is the frozen channel variant's title, original Markdown and
 * hash; the runner verifies the Rust digest algorithm without re-rendering
 * Markdown. The API remains responsible for verifying their semantic match.
 * This module does not parse images or implement a platform editor.
 */
export function stageRichPublication(payload, mediaParts, variant) {
  const distinct = validateRichPublicationPayload(payload);
  record(variant, ["title", "markdown", "payload_hash"]);
  if (
    variant.title !== payload.document.title ||
    typeof variant.markdown !== "string" ||
    Buffer.byteLength(variant.markdown, "utf8") > 8 * 1024 * 1024 ||
    typeof variant.payload_hash !== "string" ||
    !SHA256.test(variant.payload_hash) ||
    variantHash(variant.title, variant.markdown, payload) !==
      variant.payload_hash
  )
    fail("rich_payload_hash_mismatch");
  if (!Array.isArray(mediaParts) || mediaParts.length !== distinct.size)
    fail("media_parts_mismatch");
  const bytes = new Map();
  try {
    for (const part of mediaParts) {
      record(part, [
        "object_id",
        "object_version",
        "sha256",
        "media_type",
        "bytes",
      ]);
      const key = objectKey({
        object_id: part.object_id,
        object_version: part.object_version,
        sha256: part.sha256,
      });
      const item = distinct.get(key);
      if (
        !item ||
        bytes.has(key) ||
        part.sha256 !== item.object.sha256 ||
        part.media_type !== item.media_type ||
        !(part.bytes instanceof Uint8Array) ||
        part.bytes.byteLength !== item.byte_len
      )
        fail("media_parts_mismatch");
      const copy = Buffer.from(part.bytes);
      if (
        createHash("sha256").update(copy).digest("hex") !== item.object.sha256
      )
        fail("media_bytes_corrupt");
      bytes.set(key, copy);
    }
    const snapshot = freeze(structuredClone(payload));
    const stage = Object.freeze({
      payload: snapshot,
      payloadHash: variant.payload_hash,
      media: freeze(
        [...distinct.values()].map((item) => ({
          object_id: item.object.object_id,
          object_version: item.object.object_version,
          sha256: item.object.sha256,
          media_type: item.media_type,
          byte_len: item.byte_len,
        })),
      ),
    });
    stagedBytes.set(stage, bytes);
    return stage;
  } catch (error) {
    for (const copy of bytes.values()) copy.fill(0);
    throw error;
  }
}

/**
 * `authorize` is a durable, one-shot API transaction, NOT a client approval.
 * Its exact grant must be bound to this attempt, runner session and frozen
 * payload hash. On a lost response or timeout, callers reconcile the attempt;
 * this stage cannot retry or issue another upload. `upload` is the first
 * external action and owns all subsequent platform-specific behavior.
 */
export async function executeAuthorizedRichPublication(
  stage,
  {
    expected,
    authorize,
    upload,
    clock = () => Date.now(),
    authorizationTimeoutMs = 10_000,
  },
) {
  const bytes = stagedBytes.get(stage);
  if (!bytes) fail("rich_stage_consumed");
  stagedBytes.delete(stage);
  try {
    record(expected, ["attempt_id", "runner_session_id", "payload_hash"]);
    if (
      !uuid(expected.attempt_id) ||
      typeof expected.runner_session_id !== "string" ||
      !/^[a-zA-Z0-9_-]{1,128}$/.test(expected.runner_session_id) ||
      typeof expected.payload_hash !== "string" ||
      !SHA256.test(expected.payload_hash) ||
      expected.payload_hash !== stage.payloadHash ||
      typeof authorize !== "function" ||
      typeof upload !== "function" ||
      typeof clock !== "function" ||
      !positive(authorizationTimeoutMs, 30_000)
    )
      fail("invalid_send_context");
    let grant;
    let timeout;
    try {
      const deadline = new Promise((_, reject) => {
        timeout = setTimeout(
          () => reject(new Error("authorization_deadline")),
          authorizationTimeoutMs,
        );
      });
      grant = await Promise.race([
        authorize(freeze({ ...expected })),
        deadline,
      ]);
    } catch {
      fail("send_authorization_unknown");
    } finally {
      clearTimeout(timeout);
    }
    if (grant?.status === "already_consumed")
      fail("send_authorization_consumed");
    if (grant?.status === "denied") fail("send_authorization_denied");
    if (
      !grant ||
      grant.status !== "granted" ||
      Object.keys(grant).some(
        (key) =>
          ![
            "status",
            "attempt_id",
            "runner_session_id",
            "payload_hash",
            "send_not_after",
          ].includes(key),
      ) ||
      grant.attempt_id !== expected.attempt_id ||
      grant.runner_session_id !== expected.runner_session_id ||
      grant.payload_hash !== expected.payload_hash ||
      typeof grant.send_not_after !== "string" ||
      !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?Z$/.test(
        grant.send_not_after,
      ) ||
      !Number.isFinite(Date.parse(grant.send_not_after))
    )
      fail("send_authorization_unknown");
    const now = clock();
    if (!Number.isFinite(now) || now >= Date.parse(grant.send_not_after))
      fail("send_authorization_expired");
    // Buffer copies leave the stage only after authorization. Adapters must
    // avoid logging these parts or returning them as publication evidence.
    return await upload(
      stage.payload,
      [...bytes].map(([key, source]) => ({
        ...stage.media.find(
          (item) =>
            `${item.object_id.toLowerCase()}:${item.object_version}` === key,
        ),
        bytes: Buffer.from(source),
      })),
    );
  } finally {
    for (const source of bytes.values()) source.fill(0);
  }
}
