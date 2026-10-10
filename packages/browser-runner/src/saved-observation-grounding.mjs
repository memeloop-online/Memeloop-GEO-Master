import { createHash } from "node:crypto";
import {
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
const MAX_AUDIT_BYTES = 150_000;
const object = (value) =>
  value !== null && typeof value === "object" && !Array.isArray(value);

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
        ].includes(key),
    ) ||
    typeof input.source_json !== "string" ||
    typeof input.candidate_json !== "string" ||
    !object(input.protocol) ||
    typeof input.source_sha256 !== "string" ||
    !/^[a-f0-9]{64}$/u.test(input.source_sha256)
  )
    return rejected("shape_rejected");
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
    });
    if (!grounded)
      return rejected(
        observationRejectionReason(document, candidate, { semanticOnly: true }),
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
          prompt_version: "geo.observation.extract.v2",
          source_sha256: input.source_sha256,
          protocol: input.protocol,
        },
      },
    };
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
