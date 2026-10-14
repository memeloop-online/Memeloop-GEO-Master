import { createHash } from "node:crypto";
import {
  interpretObservation,
  observationAbortCode,
  ObservationPersistenceError,
  persistObservationSource,
  reportObservationDiagnostic,
} from "./ai-observation-parser.mjs";
import { probeDeepseekAccount } from "./consumer-web-identity.mjs";
import { observeDeepSeekCreatedSessions } from "./deepseek-created-sessions.mjs";
import {
  capturedConversationCompletion,
  validDeepSeekEvidence,
} from "./deepseek-conversation-evidence.mjs";
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
import { readDeepSeekFirstTurnHistory } from "./deepseek-web-history.mjs";

class FlowError extends Error {
  constructor(code) {
    super(code);
    this.code = code;
  }
}

/**
 * Inactive integration entry point: the caller owns the page/context lifetime.
 * A provider-specific ownership staging adapter must be explicitly supplied
 * before any question can be sent; transport evidence registration alone is
 * not that persistence integration.
 *
 * persistConversation receives the exact creation proof and sanitized capture,
 * plus the independently reverified account. An adapter may stage ownership
 * with {durable:false}, then atomically persist it with the source in onEvidence.
 * It must not wait here for that later checkpoint. Only {durable:true}
 * acknowledges independent ownership persistence; neither status is completion
 * or deletion authority. Failure cannot discard source or cause another send.
 *
 * Source projection is privacy-only. The shared interpreter owns all answer,
 * search and citation semantics. The source document retains messages and the
 * privacy redaction manifest and, when independently verified, history lifecycle.
 * Production registration and remote cleanup remain separate integrations.
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
    readHistory = readDeepSeekFirstTurnHistory,
    prepareSource = (exchange) =>
      projectDeepseekSource(exchange, (code) =>
        reportObservationDiagnostic(onDiagnostic, "capture", code),
      ),
    interpret = interpretObservation,
  } = {},
) {
  let stage = "input_validation";
  let ownershipReceiptStatus;
  let sourceRedaction;
  let persistenceFailed = false;
  let checkpointInFlight = false;
  let interpretationEvidenceInFlight = 0;
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
  const checkpointReserve = Math.min(
    5_000,
    Math.max(1, (overallDeadline - performance.now()) / 4),
  );
  const captureDeadline = Math.min(
    sourceDeadlineAt ?? overallDeadline,
    overallDeadline - checkpointReserve,
    performance.now() + 120_000,
  );
  let configurationObserver;
  let createdSessions;
  // Every awaited integration hook is bounded too. A late hook completion
  // never advances this flow; cancellation cannot undo an already sent action.
  const run = async (
    operation,
    end = captureDeadline,
    drainCapture = false,
  ) => {
    const controller = new AbortController();
    let timer;
    let abort;
    let drainTimer;
    const rejectDrained = (reject, code) => {
      if (checkpointInFlight) {
        persistenceFailed = true;
        reject(new ObservationPersistenceError());
      } else reject(new FlowError(code));
    };
    const check = () => {
      if (signal?.aborted) throw new FlowError(observationAbortCode(signal));
      if (performance.now() >= end) throw new FlowError("budget_exhausted");
    };
    check();
    try {
      const interrupted = new Promise((_, reject) => {
        abort = () => {
          controller.abort(signal?.reason);
          if (drainCapture) {
            drainTimer ??= setTimeout(
              () => rejectDrained(reject, observationAbortCode(signal)),
              checkpointReserve,
            );
          } else reject(new FlowError(observationAbortCode(signal)));
        };
        const expire = () => {
          const remaining = end - performance.now();
          if (remaining > 0) {
            timer = setTimeout(expire, Math.max(1, Math.ceil(remaining)));
            return;
          }
          controller.abort(
            new DOMException("Capture budget exhausted", "TimeoutError"),
          );
          if (drainCapture) {
            drainTimer ??= setTimeout(
              () => rejectDrained(reject, "budget_exhausted"),
              checkpointReserve,
            );
          } else reject(new FlowError("budget_exhausted"));
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
      clearTimeout(drainTimer);
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
    const captured = await run(
      (limits) =>
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
          onDiagnostic,
          onPartialCapture: async (prefix) => {
            const prepared = prepareSource(prefix);
            if (!prepared?.exchange)
              throw new FlowError("source_privacy_unverified");
            sourceRedaction = prepared.redaction;
            let checkpointTimer;
            checkpointInFlight = true;
            try {
              // Source checkpoint remains allowed after cancellation: it sends no
              // provider request and cannot confer ownership or cleanup rights.
              await Promise.race([
                persistObservationSource(
                  { ...prepared.exchange, source_redaction: sourceRedaction },
                  { provider: "deepseek", onEvidence },
                ),
                new Promise((_, reject) => {
                  checkpointTimer = setTimeout(
                    () => reject(new ObservationPersistenceError()),
                    Math.max(
                      1,
                      Math.min(
                        checkpointReserve,
                        overallDeadline - performance.now(),
                      ),
                    ),
                  );
                }),
              ]);
            } catch (error) {
              persistenceFailed = true;
              throw error;
            } finally {
              checkpointInFlight = false;
              clearTimeout(checkpointTimer);
            }
          },
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
      captureDeadline,
      true,
    );
    if (!captured?.created_session) return unknown("capture_unverified");
    const prepared = prepareSource(captured);
    if (!prepared?.exchange) return unknown("source_privacy_unverified");
    let exchange = prepared.exchange;
    // Keep privacy metadata separate from provider transport evidence.
    sourceRedaction = prepared.redaction;
    const questionSha256 = createHash("sha256")
      .update(payload.question, "utf8")
      .digest("hex");
    // capture() matched the actual first-turn POST against this binding and
    // revalidated its fresh browser-observed create proof after transport EOF.
    // Ownership is independent of terminal ready/finish or readable history.
    const ownership = {
      schema_version: "geo.deepseek.ownership.v1",
      provider: "deepseek",
      creation: {
        source: captured.created_session.source,
        chat_session_id: captured.created_session.chatSessionId,
      },
      submission: {
        chat_session_id: captured.created_session.chatSessionId,
        parent_message_id: null,
        model_type: payload.model,
        prompt_sha256: questionSha256,
        search_enabled: true,
        thinking_enabled: false,
      },
    };
    if (
      createdSessions?.isCurrent?.(captured.created_session) === true &&
      validDeepSeekEvidence({ ...captured, ownership }) &&
      validDeepSeekEvidence({ ...exchange, ownership })
    )
      exchange = { ...exchange, ownership: structuredClone(ownership) };
    stage = "history";
    // Only transport ready IDs select the read. Never use answer text, an
    // inferred ID, a latest-session lookup or a second question submission.
    const readyEvents = captured.messages?.filter(
      (message) => message?.event === "ready",
    );
    const ready = readyEvents?.length === 1 ? readyEvents[0].data : null;
    const validId = (id) =>
      Number.isSafeInteger(id) && id >= 0 && !Object.is(id, -0);
    if (
      captured.sse_terminal === true &&
      validId(ready?.request_message_id) &&
      validId(ready?.response_message_id) &&
      ready.request_message_id !== ready.response_message_id &&
      (ready.model_type == null || ready.model_type === payload.model) &&
      createdSessions?.isCurrent?.(captured.created_session) === true
    ) {
      try {
        // Leave at least three quarters of the remaining overall budget for
        // durable ownership/source checkpoints and the shared interpreter.
        const historyDeadline = Math.min(
          captureDeadline,
          performance.now() +
            Math.min(5_000, (overallDeadline - performance.now()) / 4),
        );
        const observed = await run(
          (limits) =>
            readHistory(page, {
              ...limits,
              conversationId: captured.created_session.chatSessionId,
              requestMessageId: ready.request_message_id,
              responseMessageId: ready.response_message_id,
            }),
          historyDeadline,
        );
        const lifecycle = {
          schema_version: "geo.deepseek.lifecycle.v1",
          provider: "deepseek",
          creation: ownership.creation,
          submission: ownership.submission,
          ready: {
            request_message_id: ready.request_message_id,
            response_message_id: ready.response_message_id,
          },
          sse_terminal: captured.sse_terminal,
          history: observed?.history,
        };
        if (
          createdSessions.isCurrent(captured.created_session) === true &&
          capturedConversationCompletion({ ...captured, lifecycle }) &&
          capturedConversationCompletion({
            ...exchange,
            source_redaction: sourceRedaction,
            lifecycle,
          })
        ) {
          // Validate against raw transport, then retain only the closed
          // metadata contract. Never restore privacy-omitted finish payloads.
          exchange = {
            ...exchange,
            source_redaction: sourceRedaction,
            lifecycle: structuredClone(lifecycle),
          };
        }
      } catch {
        // Optional history cannot discard a captured, privacy-safe source.
      }
    }
    // Never route an AI-inferred ID to the ownership ledger.
    ownershipReceiptStatus = "unpersisted";
    try {
      const ack = await run(
        (limits) =>
          persistConversation({
            createdSession: captured.created_session,
            captured: exchange,
            binding: {
              model: payload.model,
              parentMessageId: null,
              searchEnabled: true,
              thinkingEnabled: false,
              questionSha256,
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
          { ...exchange, source_redaction: sourceRedaction },
          {
            provider: "deepseek",
            deadlineAt: overallDeadline,
            signal: limits.signal,
            allowLegacyApi: false,
            apiExtract,
            getExtractionPolicy,
            onDiagnostic,
            onEvidence: async (record) => {
              interpretationEvidenceInFlight++;
              try {
                await run(() => onEvidence(record), overallDeadline);
              } catch (error) {
                persistenceFailed = true;
                throw error;
              } finally {
                interpretationEvidenceInFlight--;
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
        request_question_sha256: questionSha256,
      },
    };
  } catch (error) {
    // The interpreter and its evidence hook share a deadline. An unacknowledged
    // checkpoint is a persistence failure whichever timeout settles first.
    const reason =
      persistenceFailed ||
      interpretationEvidenceInFlight > 0 ||
      error instanceof ObservationPersistenceError
        ? "evidence_persist_failed"
        : signal?.aborted
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
