import { reportObservationDiagnostic } from "../ai-observation-parser.mjs";
import { consumerWebProviders } from "../consumer-web-providers.mjs";
import {
  observeKimiConnectSearch,
  inspectKimiMeasurementOptions,
} from "../kimi-connect-search.mjs";
import {
  CONNECTOR_VERSION,
  completedMeasurement,
  defineProvider,
  ownIdentity,
  unknown,
  unsupported,
  validMeasurementPayload,
} from "./contract.mjs";

const KIMI_ORIGIN = "https://www.kimi.com";
// Public Kimi client bundle (2026-09-29) invokes
// kimi.gateway.account.v1.UserService/GetCurrentUser via Connect JSON.
// The authenticated response has not been observed with a real account.
const KIMI_SELF = "/apiv2/kimi.gateway.account.v1.UserService/GetCurrentUser";

export function kimiIdentity(data) {
  return ownIdentity(data?.user?.id, data?.user?.nickname);
}

export async function probeKimiAccount(
  page,
  { trustedOrigin = KIMI_ORIGIN } = {},
) {
  try {
    // The interactive page must keep its URL, focus, and in-progress input.
    // Check origin in the page before touching its storage or making a request:
    // even a navigation between calls cannot send a token from another site.
    const data = await page.evaluate(
      async ({ origin, selfPath }) => {
        if (location.origin !== origin) return null;
        const accessToken = localStorage.getItem("access_token");
        const refreshToken = localStorage.getItem("refresh_token");
        if (!accessToken || !refreshToken) return null;
        // Fixed, same-origin Connect request. Tokens remain page-side.
        const response = await fetch(selfPath, {
          method: "POST",
          credentials: "same-origin",
          headers: {
            Authorization: `Bearer ${accessToken}`,
            "Content-Type": "application/json",
            "Connect-Protocol-Version": "1",
            "x-msh-platform": "web",
          },
          body: "{}",
          redirect: "error",
        });
        if (
          !response.ok ||
          response.url !== `${origin}${selfPath}` ||
          !/application\/json/i.test(response.headers.get("content-type") ?? "")
        ) {
          return null;
        }
        const text = await response.text();
        return text.length <= 128_000 ? JSON.parse(text) : null;
      },
      { origin: trustedOrigin, selfPath: KIMI_SELF },
    );
    return kimiIdentity(data);
  } catch {
    return null;
  }
}

// This parses a candidate *observation*, not proof that Kimi ran its official
// search. An authenticated account, search-mode selection and an actual search
// event must be established independently before a caller can mark it complete.
// In particular, a plausible answer with links is not search evidence.
export function extractKimiCandidateObservation(value) {
  if (
    value === null ||
    typeof value !== "object" ||
    Array.isArray(value) ||
    typeof value.raw_answer !== "string" ||
    !value.raw_answer.trim() ||
    value.raw_answer.length > 100_000 ||
    !Array.isArray(value.citations) ||
    value.citations.length > 50 ||
    (value.request_id !== undefined &&
      (typeof value.request_id !== "string" ||
        !/^[\w-]{1,128}$/u.test(value.request_id)))
  ) {
    return null;
  }
  const citations = [];
  for (const citation of value.citations) {
    if (
      citation === null ||
      typeof citation !== "object" ||
      Array.isArray(citation) ||
      typeof citation.url !== "string" ||
      citation.url.length > 2048 ||
      (citation.title !== undefined &&
        (typeof citation.title !== "string" || citation.title.length > 500))
    ) {
      return null;
    }
    let url;
    try {
      url = new URL(citation.url);
    } catch {
      return null;
    }
    if (
      url.protocol !== "https:" ||
      url.username ||
      url.password ||
      !url.hostname ||
      url.hostname === "localhost" ||
      url.hostname.endsWith(".localhost")
    ) {
      return null;
    }
    citations.push({
      url: url.href,
      ...(citation.title ? { title: citation.title } : {}),
    });
  }
  return {
    raw_answer: value.raw_answer,
    citations,
    ...(value.request_id ? { request_id: value.request_id } : {}),
    search_verified: false,
  };
}

// A live connector must supply the UI action, provider-event decoder and
// answer reader only after an authenticated browser trace establishes their
// actual semantics. This captures *candidates*, never a verified receipt.
// In particular, a same-origin response or a request ID from the answer is
// not by itself proof that the provider performed official web search.
export async function observeKimiSearchAttempt(
  page,
  question,
  {
    submit,
    decodeEvent,
    readAnswer,
    waitForCompletion,
    timeoutMs = 30_000,
    trustedOrigin = KIMI_ORIGIN,
  },
) {
  if (
    typeof question !== "string" ||
    !question.trim() ||
    typeof submit !== "function" ||
    typeof decodeEvent !== "function" ||
    typeof readAnswer !== "function" ||
    (waitForCompletion !== undefined &&
      typeof waitForCompletion !== "function") ||
    !Number.isFinite(timeoutMs) ||
    timeoutMs <= 0
  ) {
    return null;
  }
  const events = [];
  const pending = new Set();
  let closed = false;
  let timer;
  const controller = new AbortController();
  const onResponse = (response) => {
    const task = (async () => {
      try {
        const url = new URL(response.url());
        if (
          url.origin !== trustedOrigin ||
          response.status() !== 200 ||
          !/application\/json/i.test(response.headers()["content-type"] ?? "")
        ) {
          return;
        }
        const body = await response.body();
        if (body.length > 128_000) return;
        const event = decodeEvent(JSON.parse(body.toString("utf8")), response);
        if (
          !closed &&
          event &&
          typeof event.event_id === "string" &&
          /^[\w-]{1,128}$/u.test(event.event_id) &&
          typeof event.request_id === "string" &&
          /^[\w-]{1,128}$/u.test(event.request_id)
        ) {
          events.push({
            event_id: event.event_id,
            request_id: event.request_id,
            // Local observation time is not the provider's event timestamp.
            received_at: new Date().toISOString(),
          });
        }
      } catch {
        // Malformed and unrelated browser traffic is not search evidence.
      }
    })();
    pending.add(task);
    void task.finally(() => pending.delete(task));
  };
  page.on("response", onResponse);
  try {
    return await Promise.race([
      (async () => {
        await submit(page, question, controller.signal);
        if (closed) return null;
        const answer = extractKimiCandidateObservation(
          await readAnswer(page, controller.signal),
        );
        if (closed || !answer?.request_id) return null;
        // The adapter must identify provider completion, not infer it from
        // answer rendering or an arbitrary quiet period. This remains only
        // candidate collection, never proof of official search.
        if (waitForCompletion) {
          await waitForCompletion(page, answer.request_id, controller.signal);
        }
        if (closed) return null;
        while (pending.size && !closed) {
          await Promise.allSettled([...pending]);
        }
        if (closed) return null;
        const matched = events.filter(
          (event) => event.request_id === answer.request_id,
        );
        if (matched.length !== 1) return null;
        return { ...answer, candidate_search_event: matched[0] };
      })(),
      new Promise((resolve) => {
        timer = setTimeout(() => resolve(null), timeoutMs);
      }),
    ]);
  } catch {
    return null;
  } finally {
    closed = true;
    clearTimeout(timer);
    controller.abort();
    page.off("response", onResponse);
  }
}

export async function measureKimi(
  page,
  payload,
  {
    searchFlow,
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
  if (!validMeasurementPayload(payload, "kimi"))
    return unsupported("invalid_measurement_payload");
  if (typeof expectedAccountId !== "string" || !expectedAccountId.trim())
    return unsupported("account_identity_unverified");
  // Historical injected JSON candidates never confer official search proof.
  if (searchFlow) {
    const candidate = await observeKimiSearchAttempt(page, payload.question, {
      ...searchFlow,
    });
    if (!candidate)
      return unknown("official_search_event_unverified", "measure");
    return unknown("official_search_provenance_unverified", "measure");
  }
  if (!page) return unsupported("official_web_search_unverified");
  const diagnostics = [];
  const capturedEvidence = [];
  const ownershipEvidence = [];
  const observation = await observeKimiConnectSearch(page, payload, {
    deadlineAt,
    sourceDeadlineAt,
    signal,
    apiExtract,
    getExtractionPolicy,
    ...(typeof onConversationCaptured === "function"
      ? {
          onConversationCaptured: async (receipt) => {
            let ack;
            try {
              ack = await onConversationCaptured(receipt);
            } catch {
              // The ownership callback failure cannot cause a second send.
            }
            ownershipEvidence.push({
              kind: "provider_conversation_ownership",
              schema_version: "geo.provider_conversation.ownership.v1",
              provider: receipt.provider,
              purpose: receipt.purpose,
              external_conversation_id: receipt.external_conversation_id,
              ownership_receipt_status:
                ack?.durable === true ? "persisted" : "unpersisted",
            });
            return ack;
          },
        }
      : {}),
    onEvidence: async (record) => {
      capturedEvidence.push(record);
      // Supplying this callback is a caller-owned persistence contract. A
      // failed write prevents extraction from proceeding; the final-return
      // evidence alone is not a durable acknowledgement for remote cleanup.
      await onEvidence?.(record);
    },
    onDiagnostic: (entry) => {
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
  if (!observation)
    return unknown("official_search_observation_unverified", "measure", [
      ...capturedEvidence,
      ...ownershipEvidence,
      ...diagnostics,
    ]);
  return completedMeasurement(payload, observation, [
    ...capturedEvidence,
    ...ownershipEvidence,
  ]);
}

export const kimiAdapter = defineProvider({
  ...consumerWebProviders.kimi,
  inspectMeasurementOptions: inspectKimiMeasurementOptions,
  connectorVersion: CONNECTOR_VERSION,
  operations: ["measure"],
  identify(page) {
    return probeKimiAccount(page);
  },
  async execute(page, operation, payload, network) {
    if (operation !== "measure") return unsupported("operation_not_supported");
    return measureKimi(page, payload, network);
  },
});
