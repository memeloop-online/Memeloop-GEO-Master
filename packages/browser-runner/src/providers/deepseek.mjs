import { createHash } from "node:crypto";
import { reportObservationDiagnostic } from "../ai-observation-parser.mjs";
import { consumerWebProviders } from "../consumer-web-providers.mjs";
import { probeDeepseekAccount } from "../consumer-web-identity.mjs";
import { inspectDeepSeekMeasurementOptions } from "../deepseek-model-discovery.mjs";
import { observeDeepSeekWebSearch } from "../deepseek-web-search.mjs";
import { capturedNewConversationId } from "../provider-conversation-ownership.mjs";
import {
  CONNECTOR_VERSION,
  completedMeasurement,
  defineProvider,
  unknown,
  unsupported,
  validMeasurementPayload,
} from "./contract.mjs";

export async function measureDeepseek(
  page,
  payload,
  {
    expectedAccountId,
    deadlineAt,
    sourceDeadlineAt,
    signal,
    onEvidence,
    onConversationCaptured,
    apiExtract,
    getExtractionPolicy,
  } = {},
) {
  if (!validMeasurementPayload(payload, "deepseek"))
    return unsupported("invalid_measurement_payload");
  if (typeof expectedAccountId !== "string" || !expectedAccountId.trim())
    return unsupported("account_identity_unverified");
  if (!page) return unsupported("official_web_search_unverified");
  if (typeof onConversationCaptured !== "function")
    return unsupported("ownership_unavailable");
  if (typeof onEvidence !== "function")
    return unsupported("evidence_persistence_unavailable");
  const capturedEvidence = [];
  const ownershipEvidence = [];
  const diagnostics = [];
  const observation = await observeDeepSeekWebSearch(page, payload, {
    expectedAccountId,
    deadlineAt,
    sourceDeadlineAt,
    signal,
    apiExtract,
    getExtractionPolicy,
    onConversationCaptured,
    async persistConversation({
      createdSession,
      captured,
      binding,
      expectedAccountId: observedAccountId,
      signal: captureSignal,
    }) {
      const conversationId = capturedNewConversationId(captured, {
        provider: "deepseek",
      });
      const submission = captured.ownership?.submission;
      if (
        captureSignal?.aborted ||
        observedAccountId !== expectedAccountId ||
        createdSession?.source !== "browser_observed_create" ||
        !conversationId ||
        conversationId !== createdSession.chatSessionId ||
        submission?.model_type !== payload.model ||
        submission?.prompt_sha256 !== binding.questionSha256 ||
        binding.questionSha256 !==
          createHash("sha256").update(payload.question, "utf8").digest("hex") ||
        binding.model !== payload.model ||
        binding.parentMessageId !== null ||
        binding.searchEnabled !== true ||
        binding.thinkingEnabled !== false
      )
        return { durable: false };
      const receipt = {
        provider: "deepseek",
        purpose: "measurement",
        external_conversation_id: conversationId,
      };
      let ack;
      try {
        ack = await onConversationCaptured(receipt);
      } finally {
        ownershipEvidence.push({
          kind: "provider_conversation_ownership",
          schema_version: "geo.provider_conversation.ownership.v1",
          ...receipt,
          ownership_receipt_status:
            ack?.durable === true ? "persisted" : "unpersisted",
        });
      }
      // false acknowledges staging only: onEvidence commits the source and
      // ownership atomically before the interpreter may invoke extraction.
      return ack;
    },
    async onEvidence(record) {
      capturedEvidence.push(record);
      await onEvidence(record);
    },
    onDiagnostic(entry) {
      if (diagnostics.length >= 12) return;
      reportObservationDiagnostic(
        (safe) => diagnostics.push(safe),
        entry.stage,
        entry.code,
        entry.route,
      );
    },
  });
  if (observation?.reason === "requested_model_unavailable")
    return unsupported("requested_model_unavailable");
  if (
    !observation ||
    observation.reason ||
    typeof observation.raw_answer !== "string" ||
    !observation.raw_answer.trim() ||
    !Array.isArray(observation.citations) ||
    !observation.extraction_audit ||
    !observation.search_event
  )
    return unknown(
      observation?.reason ?? "official_search_observation_unverified",
      "measure",
      [...capturedEvidence, ...ownershipEvidence, ...diagnostics],
    );
  return completedMeasurement(payload, observation, [
    ...capturedEvidence,
    ...ownershipEvidence,
  ]);
}

export const deepseekAdapter = defineProvider({
  ...consumerWebProviders.deepseek,
  inspectMeasurementOptions: inspectDeepSeekMeasurementOptions,
  connectorVersion: CONNECTOR_VERSION,
  operations: ["measure"],
  identify: probeDeepseekAccount,
  async execute(page, operation, payload, network) {
    if (operation !== "measure") return unsupported("operation_not_supported");
    return measureDeepseek(page, payload, network);
  },
});
