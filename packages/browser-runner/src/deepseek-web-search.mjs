import { createHash } from "node:crypto";
import {
  interpretObservation,
  observationAbortCode,
  reportObservationDiagnostic,
} from "./ai-observation-parser.mjs";
import { probeDeepseekAccount } from "./consumer-web-identity.mjs";
import { observeDeepSeekCreatedSessions } from "./deepseek-created-sessions.mjs";
import { projectDeepseekSource } from "./deepseek-source-projection.mjs";
import { observeDeepSeekModelConfiguration } from "./deepseek-web-configuration.mjs";
import {
  configureDeepSeekPage,
  submitDeepSeekQuestion,
} from "./deepseek-web-controls.mjs";
import {
  captureDeepSeekExchange,
  DEEPSEEK_WEB_ORIGIN,
} from "./deepseek-web-measurement.mjs";

class FlowError extends Error {
  constructor(code) {
    super(code);
    this.code = code;
  }
}

/**
 * Inactive integration entry point: the caller owns the page/context lifetime.
 * A provider-specific durable ownership adapter must be explicitly supplied
 * before any question can be sent. The existing Kimi-only registry is not one.
 *
 * persistConversation receives the exact creation proof and sanitized capture,
 * plus the independently reverified account. Only {durable:true} acknowledges
 * persistence; it is not completion or deletion authority. Failure cannot
 * discard captured source or cause a second submission.
 *
 * Source projection is privacy-only. The shared interpreter owns all answer,
 * search and citation semantics. The source document retains messages and the
 * privacy redaction manifest. Durable creation/request binding and lifecycle
 * contracts must be completed before this entry point is registered.
 */
export async function observeDeepSeekWebSearch(
  page,
  payload,
  {
    expectedAccountId,
    timeoutMs = 30_000,
    deadlineAt,
    sourceDeadlineAt,
    signal,
    onConversationCaptured,
    onEvidence,
    onDiagnostic,
    apiExtract,
    getExtractionPolicy,
    persistConversation,
    identify = probeDeepseekAccount,
    observeConfiguration = observeDeepSeekModelConfiguration,
    observeCreatedSessions = observeDeepSeekCreatedSessions,
    configure = configureDeepSeekPage,
    capture = captureDeepSeekExchange,
    submitQuestion = submitDeepSeekQuestion,
    prepareSource = projectDeepseekSource,
    interpret = interpretObservation,
  } = {},
) {
  let stage = "input_validation";
  let ownershipReceiptStatus;
  let sourceRedaction;
  let persistenceFailed = false;
  const unknown = (reason) => ({
    reason,
    ...(ownershipReceiptStatus
      ? { ownership_receipt_status: ownershipReceiptStatus }
      : {}),
    ...(sourceRedaction ? { source_redaction: sourceRedaction } : {}),
  });
  if (
    !page ||
    typeof payload?.model !== "string" ||
    !/^[\w.-]{1,128}$/u.test(payload.model) ||
    typeof payload.question !== "string" ||
    !payload.question.trim() ||
    Buffer.byteLength(payload.question) > 16_000 ||
    typeof expectedAccountId !== "string" ||
    !expectedAccountId.trim() ||
    expectedAccountId.length > 128 ||
    !Number.isFinite(timeoutMs) ||
    timeoutMs <= 0 ||
    timeoutMs > 120_000 ||
    (deadlineAt !== undefined && !Number.isFinite(deadlineAt)) ||
    (sourceDeadlineAt !== undefined && !Number.isFinite(sourceDeadlineAt))
  )
    return unknown("invalid_input");
  if (signal?.aborted) return unknown(observationAbortCode(signal));
  if (typeof persistConversation !== "function")
    return unknown("ownership_unavailable");
  if (typeof onEvidence !== "function")
    return unknown("evidence_persistence_unavailable");

  const overallDeadline = deadlineAt ?? performance.now() + timeoutMs;
  const captureDeadline = Math.min(
    sourceDeadlineAt ?? overallDeadline,
    overallDeadline,
    performance.now() + 120_000,
  );
  let configurationObserver;
  let createdSessions;
  // Every awaited integration hook is bounded too. A late hook completion
  // never advances this flow; cancellation cannot undo an already sent action.
  const run = async (operation, end = captureDeadline) => {
    const controller = new AbortController();
    let timer;
    let abort;
    const check = () => {
      if (signal?.aborted) throw new FlowError(observationAbortCode(signal));
      if (performance.now() >= end) throw new FlowError("budget_exhausted");
    };
    check();
    try {
      const interrupted = new Promise((_, reject) => {
        abort = () => {
          controller.abort();
          reject(new FlowError(observationAbortCode(signal)));
        };
        const expire = () => {
          const remaining = end - performance.now();
          if (remaining > 0) {
            timer = setTimeout(expire, Math.max(1, Math.ceil(remaining)));
            return;
          }
          controller.abort();
          reject(new FlowError("budget_exhausted"));
        };
        signal?.addEventListener("abort", abort, { once: true });
        timer = setTimeout(
          expire,
          Math.max(1, Math.ceil(end - performance.now())),
        );
      });
      const result = await Promise.race([
        Promise.resolve().then(() => {
          check();
          return operation({
            signal: controller.signal,
            timeoutMs: Math.min(120_000, Math.max(1, end - performance.now())),
          });
        }),
        interrupted,
      ]);
      check();
      return result;
    } finally {
      clearTimeout(timer);
      signal?.removeEventListener("abort", abort);
      controller.abort();
    }
  };

  try {
    if (captureDeadline <= performance.now())
      return unknown("budget_exhausted");
    const observerOptions = {
      signal,
      timeoutMs: Math.max(1, captureDeadline - performance.now()),
    };
    configurationObserver = observeConfiguration(page, observerOptions);
    createdSessions = observeCreatedSessions(page, observerOptions);
    stage = "navigation";
    await run(({ timeoutMs: remaining }) =>
      page.goto(`${DEEPSEEK_WEB_ORIGIN}/`, {
        waitUntil: "domcontentloaded",
        timeout: remaining,
      }),
    );
    if (page.url() !== `${DEEPSEEK_WEB_ORIGIN}/`)
      return unknown("navigation_unverified");
    const identity = await run(() => identify(page));
    if (identity?.platform_account_id !== expectedAccountId)
      return unknown("account_identity_unverified");
    stage = "configuration";
    const configuration = await run(() => configurationObserver.result);
    if (!configuration) return unknown("configuration_unverified");
    if (
      !configuration.model_configs?.some(
        (model) =>
          model.model_type === payload.model &&
          model.enabled === true &&
          model.switchable === true,
      )
    )
      return unknown("requested_model_unavailable");
    const configured = await run((limits) =>
      configure(page, {
        ...limits,
        model: payload.model,
        modelConfigs: configuration.model_configs,
        searchEnabled: true,
        thinkingEnabled: false,
      }),
    );
    if (
      configured?.model !== payload.model ||
      configured.searchEnabled !== true ||
      configured.thinkingEnabled !== false
    )
      return unknown("configuration_unverified");
    stage = "capture";
    let submitted = false;
    const captured = await run((limits) =>
      capture(page, {
        ...limits,
        binding: {
          question: payload.question,
          model: payload.model,
          parentMessageId: null,
          searchEnabled: true,
          thinkingEnabled: false,
        },
        createdSessions,
        submit: async (_page, captureSignal) => {
          if (submitted) throw new FlowError("submission_already_attempted");
          submitted = true;
          if (captureSignal?.aborted || limits.signal.aborted)
            throw new FlowError("cancelled");
          if (performance.now() >= captureDeadline)
            throw new FlowError("budget_exhausted");
          return submitQuestion(page, {
            question: payload.question,
            signal: captureSignal ?? limits.signal,
            timeoutMs: Math.max(1, captureDeadline - performance.now()),
          });
        },
      }),
    );
    if (!captured?.created_session) return unknown("capture_unverified");
    const prepared = prepareSource(captured);
    if (!prepared?.exchange) return unknown("source_privacy_unverified");
    // Keep privacy metadata separate from provider transport evidence.
    sourceRedaction = prepared.redaction;
    // Never route an AI-inferred ID to the ownership ledger.
    ownershipReceiptStatus = "unpersisted";
    try {
      const ack = await run(
        (limits) =>
          persistConversation({
            createdSession: captured.created_session,
            captured: prepared.exchange,
            binding: {
              model: payload.model,
              parentMessageId: null,
              searchEnabled: true,
              thinkingEnabled: false,
              questionSha256: createHash("sha256")
                .update(payload.question, "utf8")
                .digest("hex"),
            },
            expectedAccountId,
            onConversationCaptured,
            signal: limits.signal,
          }),
        Math.min(
          overallDeadline,
          performance.now() +
            Math.min(5_000, (overallDeadline - performance.now()) / 2),
        ),
      );
      if (ack?.durable === true) ownershipReceiptStatus = "persisted";
    } catch {
      // The source checkpoint is independent and must still be attempted.
    }
    stage = "interpretation";
    const observation = await run(
      (limits) =>
        interpret(
          { ...prepared.exchange, source_redaction: sourceRedaction },
          {
            provider: "deepseek",
            deadlineAt: overallDeadline,
            signal: limits.signal,
            allowLegacyApi: false,
            apiExtract,
            getExtractionPolicy,
            onDiagnostic,
            onEvidence: async (record) => {
              try {
                await run(() => onEvidence(record), overallDeadline);
              } catch (error) {
                persistenceFailed = true;
                throw error;
              }
            },
          },
        ),
      overallDeadline,
    );
    if (!observation)
      return unknown(
        persistenceFailed
          ? "evidence_persist_failed"
          : "interpretation_unverified",
      );
    return {
      raw_answer: observation.raw_answer,
      citations: observation.citations,
      extraction_audit: observation.audit,
      ownership_receipt_status: ownershipReceiptStatus,
      source_redaction: sourceRedaction,
      search_event: {
        kind: "official_search_event",
        source: "browser_response_ai",
        provenance: "live",
        search_used: "yes",
        extraction_model: observation.audit.model,
        extraction_prompt_version: observation.audit.prompt_version,
        source_sha256: observation.audit.source_sha256,
        observed_at: captured.received_at,
        request_model: payload.model,
        request_question_sha256: createHash("sha256")
          .update(payload.question, "utf8")
          .digest("hex"),
      },
    };
  } catch (error) {
    const reason = signal?.aborted
      ? observationAbortCode(signal)
      : error instanceof FlowError
        ? error.code
        : error?.message === "source_privacy_unverified"
          ? "source_privacy_unverified"
          : "observation_unverified";
    reportObservationDiagnostic(
      onDiagnostic,
      stage,
      ["cancelled", "budget_exhausted"].includes(reason)
        ? reason
        : "unverified",
    );
    return unknown(reason);
  } finally {
    configurationObserver?.dispose();
    createdSessions?.dispose();
  }
}
