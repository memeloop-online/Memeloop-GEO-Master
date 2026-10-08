import { createHash } from "node:crypto";

const ID = /^[A-Za-z0-9_-]{1,128}$/u;
const MAX_ANSWER_BYTES = 100_000;
const MAX_AUDIT_BYTES = 512;
const MAX_SEGMENTS = 32_768;
const MAX_CITATIONS = 50;

const object = (value) =>
  value !== null && typeof value === "object" && !Array.isArray(value);

function pointer(document, path) {
  if (typeof path !== "string" || !path.startsWith("/") || path.length > 2048)
    return null;
  let current = document;
  for (const escaped of path.slice(1).split("/")) {
    if (/~(?![01])/u.test(escaped)) return null;
    const key = escaped.replaceAll("~1", "/").replaceAll("~0", "~");
    if (["__proto__", "prototype", "constructor"].includes(key)) return null;
    if (Array.isArray(current)) {
      if (!/^(?:0|[1-9]\d*)$/u.test(key)) return null;
      const index = Number(key);
      if (!Number.isSafeInteger(index) || index >= current.length) return null;
      current = current[index];
    } else if (object(current) && Object.hasOwn(current, key)) {
      current = current[key];
    } else {
      return null;
    }
  }
  return { path, value: current };
}

function nonempty(value) {
  if (typeof value === "string") return value.trim().length > 0;
  if (typeof value === "number") return Number.isFinite(value) && value !== 0;
  if (typeof value === "boolean") return value === true;
  if (Array.isArray(value)) return value.length > 0;
  return object(value) && Object.keys(value).length > 0;
}

function publicUrl(value) {
  if (typeof value !== "string" || Buffer.byteLength(value, "utf8") > 2048)
    return false;
  try {
    const url = new URL(value);
    const host = url.hostname.toLowerCase();
    if (
      !["http:", "https:"].includes(url.protocol) ||
      !host ||
      url.username ||
      url.password ||
      host === "localhost" ||
      host.endsWith(".localhost") ||
      host.startsWith("[") ||
      !host.includes(".") ||
      host.endsWith(".local") ||
      host.endsWith(".test") ||
      host.endsWith(".invalid")
    )
      return false;
    const ipv4 = host.match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/u);
    if (ipv4) {
      const parts = ipv4.slice(1).map(Number);
      if (
        parts.some((part) => part > 255) ||
        parts[0] === 0 ||
        parts[0] === 10 ||
        parts[0] === 127 ||
        parts[0] >= 224 ||
        (parts[0] === 100 && parts[1] >= 64 && parts[1] <= 127) ||
        (parts[0] === 169 && parts[1] === 254) ||
        (parts[0] === 172 && parts[1] >= 16 && parts[1] <= 31) ||
        (parts[0] === 192 && (parts[1] === 0 || parts[1] === 168)) ||
        (parts[0] === 198 && [18, 19, 51].includes(parts[1])) ||
        (parts[0] === 203 && parts[1] === 0 && parts[2] === 113)
      )
        return false;
    }
    return true;
  } catch {
    return false;
  }
}

function auditValue(value) {
  let encoded;
  try {
    encoded = JSON.stringify(value);
  } catch {
    return null;
  }
  if (encoded === undefined) return null;
  const bytes = Buffer.byteLength(encoded, "utf8");
  if (bytes <= MAX_AUDIT_BYTES) return { value };
  let preview = "";
  for (const character of encoded) {
    if (Buffer.byteLength(preview + character, "utf8") > MAX_AUDIT_BYTES) break;
    preview += character;
  }
  return {
    preview,
    byte_length: bytes,
    sha256: createHash("sha256").update(encoded, "utf8").digest("hex"),
  };
}

/**
 * Fixed-vocabulary explanation of a rejected candidate. This is diagnostic
 * only: the validator below remains the sole authority for acceptance.
 */
export function observationRejectionReason(document, extracted) {
  if (extracted?.decision === "unverified") return "model_unverified";
  if (
    !object(document) ||
    !Array.isArray(document.messages) ||
    !object(extracted) ||
    extracted.decision !== "searched_answer" ||
    !Array.isArray(extracted.answer_segments) ||
    extracted.answer_segments.length < 1 ||
    extracted.answer_segments.length > MAX_SEGMENTS ||
    !Array.isArray(extracted.citations) ||
    extracted.citations.length > MAX_CITATIONS
  )
    return "shape_rejected";
  const required = [
    "chat_id",
    "message_id",
    "answer_owner",
    "search_owner",
    "search_block_id",
    "completion",
    "search_activity",
  ];
  const sources = [
    ...required.map((key) => extracted[key]),
    ...extracted.answer_segments,
    ...extracted.citations.flatMap((citation) =>
      object(citation) ? [citation.url, citation.usage] : [citation],
    ),
  ];
  if (sources.some((entry) => !object(entry) || !pointer(document, entry.path)))
    return "path_rejected";
  const selected = (entry) => pointer(document, entry.path)?.value;
  const messageId = selected(extracted.message_id);
  if (
    selected(extracted.answer_owner) !== messageId ||
    selected(extracted.search_owner) !== messageId ||
    selected(extracted.search_block_id) === messageId
  )
    return "owner_rejected";
  if (
    extracted.answer_segments.some((entry) => {
      if (entry.quote === undefined) return false;
      const source = selected(entry);
      return (
        typeof source !== "string" ||
        typeof entry.quote !== "string" ||
        !entry.quote ||
        source.indexOf(entry.quote) < 0 ||
        source.indexOf(entry.quote) !== source.lastIndexOf(entry.quote)
      );
    })
  )
    return "quote_rejected";
  if (
    extracted.citations.some((entry) => {
      const source = selected(entry.url);
      const quote = entry.url.quote;
      const url = quote === undefined ? source : quote;
      return (
        !publicUrl(url) ||
        (quote !== undefined &&
          (typeof source !== "string" ||
            source.indexOf(quote) < 0 ||
            source.indexOf(quote) !== source.lastIndexOf(quote)))
      );
    })
  )
    return "citation_rejected";
  return "grounding_rejected";
}

/**
 * Ground a model's proposed observation in the original framed response and,
 * optionally, read-only page text. This proves source paths and exact bytes,
 * not the model's semantic judgment that those fields mean official search.
 */
export function validateAiObservation(document, extracted) {
  if (extracted?.decision === "unverified") return null;
  if (
    !object(document) ||
    !Array.isArray(document.messages) ||
    !object(extracted) ||
    extracted.decision !== "searched_answer" ||
    !Array.isArray(extracted.answer_segments) ||
    extracted.answer_segments.length < 1 ||
    extracted.answer_segments.length > MAX_SEGMENTS ||
    !Array.isArray(extracted.citations) ||
    extracted.citations.length > MAX_CITATIONS
  )
    return null;

  const refs = [];
  const resolve = (candidate, role, valid = nonempty, allowQuote = false) => {
    if (!object(candidate) || typeof candidate.path !== "string") return null;
    const found = pointer(document, candidate.path);
    if (!found) return null;
    let selected = found.value;
    if (allowQuote && Object.hasOwn(candidate, "quote")) {
      if (
        typeof selected !== "string" ||
        typeof candidate.quote !== "string" ||
        !candidate.quote ||
        !selected.includes(candidate.quote) ||
        selected.indexOf(candidate.quote) !==
          selected.lastIndexOf(candidate.quote)
      )
        return null;
      selected = candidate.quote;
    }
    if (!valid(selected)) return null;
    const value = auditValue(selected);
    if (!value) return null;
    refs.push({
      role,
      path: found.path,
      ...(allowQuote && candidate.quote !== undefined
        ? { quote: candidate.quote }
        : {}),
      ...value,
    });
    return selected;
  };
  const validId = (value) => typeof value === "string" && ID.test(value);
  const chatId = resolve(extracted.chat_id, "chat_id", validId);
  const messageId = resolve(extracted.message_id, "message_id", (value) =>
    validId(value),
  );
  const answerOwner = resolve(extracted.answer_owner, "answer_owner", (value) =>
    validId(value),
  );
  const searchOwner = resolve(extracted.search_owner, "search_owner", (value) =>
    validId(value),
  );
  const blockId = resolve(
    extracted.search_block_id,
    "search_block_id",
    (value) => validId(value),
  );
  if (
    !chatId ||
    !messageId ||
    !blockId ||
    blockId === messageId ||
    answerOwner !== messageId ||
    searchOwner !== messageId ||
    resolve(extracted.completion, "completion") === null ||
    resolve(extracted.search_activity, "search_activity") === null
  )
    return null;

  const segments = [];
  for (const segment of extracted.answer_segments) {
    if (!object(segment)) return null;
    const source = resolve(
      segment,
      "answer_source",
      (value) => typeof value === "string" && value.length > 0,
    );
    if (source === null) return null;
    const hasQuote = Object.hasOwn(segment, "quote");
    const hasOffset =
      Object.hasOwn(segment, "start") || Object.hasOwn(segment, "end");
    if (hasQuote && hasOffset) return null;
    let start = 0;
    let end = source.length;
    if (hasQuote) {
      if (typeof segment.quote !== "string" || !segment.quote) return null;
      start = source.indexOf(segment.quote);
      if (start < 0 || source.lastIndexOf(segment.quote) !== start) return null;
      end = start + segment.quote.length;
    } else if (hasOffset) {
      if (
        !Number.isSafeInteger(segment.start) ||
        !Number.isSafeInteger(segment.end) ||
        segment.start < 0 ||
        segment.end > source.length ||
        segment.start >= segment.end
      )
        return null;
      start = segment.start;
      end = segment.end;
    }
    const exact = source.slice(start, end);
    if (
      !exact.length ||
      /[\uD800-\uDBFF]$/u.test(source.slice(0, start)) ||
      /^[\uDC00-\uDFFF]/u.test(source.slice(start)) ||
      /[\uD800-\uDBFF]$/u.test(exact)
    )
      return null;
    segments.push(exact);
    refs.push({
      role: "answer_segment",
      path: segment.path,
      start,
      end,
      ...auditValue(exact),
    });
  }
  const rawAnswer = segments.join("");
  if (
    !rawAnswer.trim() ||
    Buffer.byteLength(rawAnswer, "utf8") > MAX_ANSWER_BYTES
  )
    return null;

  const citations = [];
  for (const citation of extracted.citations) {
    if (!object(citation)) return null;
    const url = resolve(citation.url, "citation_url", publicUrl, true);
    const usage = resolve(citation.usage, "citation_usage");
    if (!url || usage === null) return null;
    if (!citations.includes(url)) citations.push(url);
  }
  return {
    raw_answer: rawAnswer,
    citations,
    chat_id: chatId,
    message_id: messageId,
    block_id: blockId,
    audit: { method: "llm_grounded", refs },
  };
}
