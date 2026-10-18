import { createHash } from "node:crypto";
import {
  extractionPrompt,
  observationDocument,
  parseExtractionJson,
} from "./ai-observation-parser.mjs";
import {
  observationRejectionReason,
  validateAiObservation,
} from "./ai-observation-grounding.mjs";

export const SAVED_GROUNDING_BODY_BYTES = 6_000_000;
const MAX_SOURCE_BYTES = 750_000;
const MAX_CANDIDATE_BYTES = 150_000;
const MAX_PROTOCOL_BYTES = 8192;
const MAX_AUDIT_BYTES = 512_000;
const MAX_PROMPT_BYTES = 1_500_000;
const PROMPT_VERSION = "geo.observation.extract.v4";
const PARSER_VERSION = "geo.observation.extract.v6";
const object = (value) =>
  value !== null && typeof value === "object" && !Array.isArray(value);

/** Prepare the current extraction prompt from exact saved source bytes only. */
export function prepareSavedObservation(input) {
  const rejected = (error) => ({ error });
  if (
    !object(input) ||
    Object.keys(input).length !== 4 ||
    Object.keys(input).some(
      (key) =>
        ![
          "source_json",
          "source_sha256",
          "prompt_version",
          "parser_version",
        ].includes(key),
    ) ||
    typeof input.source_json !== "string" ||
    typeof input.source_sha256 !== "string" ||
    !/^[a-f0-9]{64}$/u.test(input.source_sha256)
  )
    return rejected("shape_rejected");
  if (
    input.prompt_version !== PROMPT_VERSION ||
    input.parser_version !== PARSER_VERSION
  )
    return rejected("version_rejected");
  if (Buffer.byteLength(input.source_json, "utf8") > MAX_SOURCE_BYTES)
    return rejected("source_too_large");
  if (
    createHash("sha256").update(input.source_json, "utf8").digest("hex") !==
    input.source_sha256
  )
    return rejected("source_digest_mismatch");
  let document;
  try {
    document = JSON.parse(input.source_json);
  } catch {
    return rejected("source_invalid_json");
  }
  if (
    !object(document) ||
    !Array.isArray(document.messages) ||
    (Object.hasOwn(document, "rendered_text") &&
      typeof document.rendered_text !== "string")
  )
    return rejected("shape_rejected");
  try {
    // Keep saved message indices and capture/redaction metadata unchanged.
    const prompt = extractionPrompt(document);
    if (Buffer.byteLength(prompt, "utf8") > MAX_PROMPT_BYTES)
      return rejected("prompt_too_large");
    return {
      prompt,
      source_sha256: input.source_sha256,
      prompt_version: PROMPT_VERSION,
      parser_version: PARSER_VERSION,
    };
  } catch {
    return rejected("shape_rejected");
  }
}

/**
 * Pure replay of a proposed extraction against exact persisted source bytes.
 * No session, browser, model call, or provider-field semantic interpretation.
 * The authenticated caller owns tenant/task/protocol binding and persistence.
 */
export function groundSavedObservation(input) {
  let candidateJson = null;
  const rejected = (reason) => ({
    candidate_json: candidateJson,
    outcome: { status: "unverified", reason },
  });
  if (
    !object(input) ||
    Object.keys(input).some(
      (key) =>
        ![
          "source_json",
          "source_sha256",
          "candidate_json",
          "protocol",
          "prompt_version",
          "parser_version",
        ].includes(key),
    ) ||
    typeof input.source_json !== "string" ||
    typeof input.candidate_json !== "string" ||
    !object(input.protocol) ||
    typeof input.source_sha256 !== "string" ||
    !/^[a-f0-9]{64}$/u.test(input.source_sha256)
  )
    return rejected("shape_rejected");
  const promptVersion = input.prompt_version;
  if (
    promptVersion !== PROMPT_VERSION ||
    input.parser_version !== PARSER_VERSION
  )
    return rejected("version_rejected");
  if (Buffer.byteLength(input.source_json, "utf8") > MAX_SOURCE_BYTES)
    return rejected("source_too_large");
  if (Buffer.byteLength(input.candidate_json, "utf8") > MAX_CANDIDATE_BYTES)
    return rejected("candidate_too_large");
  if (
    Buffer.byteLength(JSON.stringify(input.protocol), "utf8") >
    MAX_PROTOCOL_BYTES
  )
    return rejected("protocol_too_large");
  if (
    createHash("sha256").update(input.source_json, "utf8").digest("hex") !==
    input.source_sha256
  )
    return rejected("source_digest_mismatch");
  let document;
  try {
    document = JSON.parse(input.source_json);
  } catch {
    return rejected("source_invalid_json");
  }
  const candidate = parseExtractionJson(input.candidate_json);
  if (!object(candidate)) return rejected("candidate_invalid_json");
  try {
    // Match live candidate retention. Validate the ORIGINAL proposal below:
    // deleting forbidden fields must never turn a rejected proposal valid.
    candidateJson = JSON.stringify(
      observationDocument({ messages: [candidate] }).messages[0],
    );
  } catch {
    return rejected("shape_rejected");
  }
  // Normalization can expand JSON number spellings; bound persisted bytes too.
  if (Buffer.byteLength(candidateJson, "utf8") > MAX_CANDIDATE_BYTES) {
    candidateJson = null;
    return rejected("candidate_too_large");
  }
  try {
    const grounded = validateAiObservation(document, candidate, {
      semanticOnly: true,
      allowRanges: true,
      allowRecommendations: true,
    });
    if (!grounded)
      return rejected(
        observationRejectionReason(document, candidate, {
          semanticOnly: true,
          allowRanges: true,
          allowRecommendations: true,
        }),
      );
    const result = {
      candidate_json: candidateJson,
      outcome: {
        status: "grounded",
        raw_answer: grounded.raw_answer,
        citations: grounded.citations,
        audit: {
          ...grounded.audit,
          kind: "observation_extraction",
          prompt_version: promptVersion,
          source_sha256: input.source_sha256,
          protocol: input.protocol,
        },
      },
    };
    // Optional recommendations must not displace grounded answer provenance.
    // Retain the original proposal above; only the projected analysis is omitted.
    if (
      Buffer.byteLength(JSON.stringify(result.outcome.audit), "utf8") >
      MAX_AUDIT_BYTES
    ) {
      result.outcome.audit.recommendations = {
        version: "geo.recommendations.v1",
        status: "unverified",
        items: [],
      };
      if (
        Buffer.byteLength(JSON.stringify(result.outcome.audit), "utf8") >
        MAX_AUDIT_BYTES
      )
        delete result.outcome.audit.recommendations;
    }
    // Repeated references may amplify a small candidate into a large audit.
    if (
      Buffer.byteLength(JSON.stringify(result.outcome.audit), "utf8") >
      MAX_AUDIT_BYTES
    )
      return rejected("audit_too_large");
    return result;
  } catch {
    // Never return source data or exception messages in rejection diagnostics.
    return rejected("grounding_rejected");
  }
}
