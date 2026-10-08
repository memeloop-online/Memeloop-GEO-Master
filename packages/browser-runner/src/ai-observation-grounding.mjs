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
export function observationRejectionReason(document, extracted, options = {}) {
  let reason;
  validateObservation(
    document,
    extracted,
    (code) => {
      reason ??= code;
      return null;
    },
    options,
  );
  return reason ?? "grounding_rejected";
}

/**
 * Ground a model's proposed observation in the original framed response and,
 * optionally, read-only page text. This proves source paths and exact bytes,
 * not the model's semantic judgment that those fields mean official search.
 */
export function validateAiObservation(document, extracted, options = {}) {
  return validateObservation(document, extracted, () => null, options);
}

// Acceptance and diagnostics share the exact checks; the callback receives
// fixed vocabulary only, never source values, paths, quotes or identifiers.
function validateObservation(
  document,
  extracted,
  reject,
  { semanticOnly = false } = {},
) {
  // Historical v3 replay remains available, but live interpretation explicitly
  // requires the semantic schema and cannot accept the old identifier contract.
  const semantic = semanticOnly || !Object.hasOwn(extracted ?? {}, "decision");
  if (!semantic && extracted?.decision === "unverified")
    return reject("model_unverified");
  if (
    !object(document) ||
    !Array.isArray(document.messages) ||
    !object(extracted) ||
    (!semantic && extracted.decision !== "searched_answer") ||
    !Array.isArray(extracted.answer_segments) ||
    (!semantic && extracted.answer_segments.length < 1) ||
    extracted.answer_segments.length > MAX_SEGMENTS ||
    !Array.isArray(extracted.citations) ||
    extracted.citations.length > MAX_CITATIONS
  )
    return reject("shape_rejected");
  if (semantic) {
    const keys = new Set([
      "completion",
      "completion_evidence",
      "search_used",
      "search_evidence",
      "answer_segments",
      "citations",
    ]);
    if (
      Object.keys(extracted).some((key) => !keys.has(key)) ||
      !["complete", "incomplete", "unknown"].includes(extracted.completion) ||
      !["yes", "no", "unknown"].includes(extracted.search_used) ||
      !Array.isArray(extracted.completion_evidence) ||
      !Array.isArray(extracted.search_evidence) ||
      extracted.completion_evidence.length > 256 ||
      extracted.search_evidence.length > 256
    )
      return reject("shape_rejected");
    if (extracted.completion !== "complete" || extracted.search_used !== "yes")
      return reject("model_unverified");
    if (
      !extracted.completion_evidence.length ||
      !extracted.search_evidence.length
    )
      return reject("evidence_empty");
  }

  const refs = [];
  // Scalars including false/0 can be exact evidence; interpreting their meaning
  // belongs to the model, not a provider-field/status allowlist.
  const selectedEvidence = (value) =>
    typeof value === "number"
      ? Number.isFinite(value)
      : typeof value === "boolean"
        ? true
        : nonempty(value);
  const resolve = (
    candidate,
    role,
    valid = nonempty,
    allowQuote = false,
    invalidReason = "evidence_empty",
  ) => {
    if (!object(candidate) || typeof candidate.path !== "string")
      return reject("path_rejected");
    if (
      semantic &&
      Object.keys(candidate).some((key) => !["path", "quote"].includes(key))
    )
      return reject("shape_rejected");
    const found = pointer(document, candidate.path);
    if (!found) return reject("path_rejected");
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
        return reject(
          role === "citation_url" ? "citation_rejected" : "quote_rejected",
        );
      selected = candidate.quote;
    }
    if (!valid(selected)) return reject(invalidReason);
    const value = auditValue(selected);
    if (!value) return reject("shape_rejected");
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
  let chatId, messageId, blockId;
  if (semantic) {
    for (const [entries, role] of [
      [extracted.completion_evidence, "completion"],
      [extracted.search_evidence, "search_activity"],
    ]) {
      for (const entry of entries)
        if (resolve(entry, role, selectedEvidence, true) === null) return null;
    }
  } else {
    const validId = (value) => typeof value === "string" && ID.test(value);
    const resolveId = (candidate, role) =>
      resolve(candidate, role, validId, false, "identifier_rejected");
    chatId = resolveId(extracted.chat_id, "chat_id");
    messageId = resolveId(extracted.message_id, "message_id");
    const answerOwner = resolveId(extracted.answer_owner, "answer_owner");
    const searchOwner = resolveId(extracted.search_owner, "search_owner");
    blockId = resolveId(extracted.search_block_id, "search_block_id");
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
      return reject("owner_rejected");
  }

  const segments = [];
  for (const segment of extracted.answer_segments) {
    if (!object(segment)) return reject("shape_rejected");
    const source = resolve(
      segment,
      "answer_source",
      (value) => typeof value === "string" && value.length > 0,
      false,
      "answer_type_rejected",
    );
    if (source === null) return null;
    const hasQuote = Object.hasOwn(segment, "quote");
    const hasOffset =
      Object.hasOwn(segment, "start") || Object.hasOwn(segment, "end");
    if (hasQuote && hasOffset) return reject("answer_bounds_rejected");
    let start = 0;
    let end = source.length;
    if (hasQuote) {
      if (typeof segment.quote !== "string" || !segment.quote)
        return reject("quote_rejected");
      start = source.indexOf(segment.quote);
      if (start < 0 || source.lastIndexOf(segment.quote) !== start)
        return reject("quote_rejected");
      end = start + segment.quote.length;
    } else if (hasOffset) {
      if (
        !Number.isSafeInteger(segment.start) ||
        !Number.isSafeInteger(segment.end) ||
        segment.start < 0 ||
        segment.end > source.length ||
        segment.start >= segment.end
      )
        return reject("answer_bounds_rejected");
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
      return reject("unicode_rejected");
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
  if (!rawAnswer.trim()) return reject("answer_empty");
  if (Buffer.byteLength(rawAnswer, "utf8") > MAX_ANSWER_BYTES)
    return reject("answer_too_large");

  const citations = [];
  for (const citation of extracted.citations) {
    if (!object(citation)) return reject("shape_rejected");
    if (
      semantic &&
      Object.keys(citation).some((key) => !["url", "usage"].includes(key))
    )
      return reject("shape_rejected");
    const url = resolve(
      citation.url,
      "citation_url",
      publicUrl,
      true,
      "citation_rejected",
    );
    const usage = resolve(
      citation.usage,
      "citation_usage",
      semantic ? selectedEvidence : nonempty,
      semantic,
    );
    if (!url || usage === null) return null;
    if (!citations.includes(url)) citations.push(url);
  }
  return {
    raw_answer: rawAnswer,
    citations,
    ...(semantic
      ? { completion: "complete", search_used: "yes" }
      : { chat_id: chatId, message_id: messageId, block_id: blockId }),
    audit: { method: "llm_grounded", refs },
  };
}
