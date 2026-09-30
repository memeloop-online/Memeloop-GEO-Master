import { createAgentToolLoopRunner } from "memeloop/loop-api";

const DEFAULT_MODEL_ID = "geo-default";
const DEFAULT_PROVIDER_ID = "geo-host";
const LOCAL_NODE_ID = "geo-embedded-worker";
const sessions = new Map();

/**
 * Run one agent turn through MemeLoop's portable direct AgentToolLoop runner.
 *
 * The Rust host passes snake_case fields because they are its declared bundle
 * contract. The in-process store only satisfies MemeLoop's conversation
 * protocol while this one invocation is running; it is not a replacement for
 * the Rust-owned Conversation/Message/Run persistence boundary.
 */
export async function main(input) {
  const turn = normalizeTurnInput(input);
  const host = resolveHost();
  const session = getSession(turn.conversationId);
  const modelId = turn.model ?? DEFAULT_MODEL_ID;
  const provider = createHostProvider(host, modelId);
  const definition = createDefinition(modelId);
  const context = createContext({ session, definition, provider, modelId });
  const runner = createAgentToolLoopRunner(context);

  for await (const _step of runner({
    conversationId: turn.conversationId,
    message: turn.prompt,
    runId: turn.runId,
    userMessage: {
      content: turn.prompt,
      messageId: turn.turnId,
      turnId: turn.turnId,
      timestamp: turn.timestamp,
    },
  })) {
    // The direct runner is intentionally consumed to completion. It persists
    // canonical user/assistant messages in the protocol adapter below.
  }

  const assistant = session.latestAssistant(turn.turnId);
  if (!assistant) {
    throw new Error(
      "MemeLoop completed without persisting an assistant message.",
    );
  }

  const completion = {
    answer: assistant.content,
    conversation_id: turn.conversationId,
    model: provider.lastModel ?? modelId,
    run_id: turn.runId,
    turn_id: turn.turnId,
  };
  await host.emit("loop.completed", JSON.stringify(completion));
  return completion;
}

function normalizeTurnInput(input) {
  if (!isRecord(input)) {
    throw new TypeError("Agent turn input must be an object.");
  }

  const conversationId = requiredString(
    input.conversation_id,
    "conversation_id",
  );
  const prompt = requiredString(input.prompt, "prompt");
  const turnId = requiredString(input.turn_id, "turn_id");
  const runId = requiredString(input.run_id, "run_id");
  const model =
    input.model === undefined
      ? undefined
      : requiredString(input.model, "model");
  const timestamp =
    input.timestamp === undefined
      ? Date.now()
      : requiredPositiveSafeInteger(input.timestamp, "timestamp");

  return { conversationId, model, prompt, runId, timestamp, turnId };
}

function createContext({ session, definition, modelId, provider }) {
  return {
    agentToolLoop: {
      enableToolLoop: false,
      maxIterations: 1,
    },
    defaultModelConfig: definition.modelConfig,
    llmProvider: provider,
    localNodeId: LOCAL_NODE_ID,
    modelProviderRegistry: {
      get: (providerId) =>
        providerId === DEFAULT_PROVIDER_ID ? provider : undefined,
      getConfig: () => undefined,
      list: () => [DEFAULT_PROVIDER_ID],
      listConfigs: () => [],
      resolve: (providerId, requestedModelId) => {
        if (
          providerId !== DEFAULT_PROVIDER_ID ||
          requestedModelId !== modelId
        ) {
          throw new Error(
            "The embedded host only exposes its configured model route.",
          );
        }
        return {
          apiMode: "chat-completions",
          modelId,
          provider,
          providerId: DEFAULT_PROVIDER_ID,
          wireModelId: modelId,
        };
      },
    },
    network: NOOP_LIFECYCLE,
    resolveAgentDefinition: async (definitionId) =>
      definitionId === definition.id ? definition : null,
    storage: session.storage,
    syncAdapters: [],
    tools: EMPTY_TOOL_REGISTRY,
  };
}

function createDefinition(modelId) {
  return {
    description: "Rust-hosted MemeLoop agent turn",
    id: "geo-embedded-agent",
    modelConfig: {
      modelId,
      providerId: DEFAULT_PROVIDER_ID,
    },
    name: "GEO embedded agent",
    systemPrompt: "",
    tools: [],
    version: "1",
  };
}

function createHostProvider(host, configuredModel) {
  const provider = {
    lastModel: undefined,
    modelId: configuredModel,
    name: "geo-host-bridge",
    async *chat(request) {
      const response = await host.modelComplete({
        max_output_tokens: request.maxOutputTokens,
        model:
          configuredModel === DEFAULT_MODEL_ID ? undefined : configuredModel,
        prompt: promptFromMessages(request.messages),
        system: systemFromMessages(request.messages),
      });
      assertModelCompletion(response);
      provider.lastModel = response.model;

      yield {
        id: "geo-host-completion",
        text: response.text,
        type: "text-delta",
      };
      yield {
        inputTokens: response.prompt_tokens,
        outputTokens: response.completion_tokens,
        totalTokens: response.prompt_tokens + response.completion_tokens,
        type: "usage",
      };
      yield {
        finishReason: response.finish_reason,
        type: "finish",
      };
    },
  };
  return provider;
}

function promptFromMessages(messages) {
  return messages
    .filter((message) => message.role !== "system")
    .map((message) => `${message.role}: ${messageContent(message.content)}`)
    .join("\n\n");
}

function systemFromMessages(messages) {
  const system = messages
    .filter((message) => message.role === "system")
    .map((message) => messageContent(message.content))
    .filter((message) => message.length > 0)
    .join("\n\n");
  return system.length === 0 ? undefined : system;
}

function messageContent(content) {
  if (typeof content === "string") {
    return content;
  }
  if (!Array.isArray(content)) {
    throw new TypeError("MemeLoop produced an unsupported model message.");
  }
  return content
    .map((part) => {
      if (part.type === "text" || part.type === "reasoning") {
        return part.text;
      }
      if (part.type === "tool-call") {
        return `[tool call ${part.toolName}: ${JSON.stringify(part.input)}]`;
      }
      if (part.type === "tool-result") {
        return `[tool result ${part.toolName}: ${JSON.stringify(part.output)}]`;
      }
      if (part.type === "file" || part.type === "image") {
        return `[${part.type} attachment]`;
      }
      throw new TypeError(
        "MemeLoop produced an unsupported model content part.",
      );
    })
    .join("\n");
}

function assertModelCompletion(value) {
  if (
    !isRecord(value) ||
    typeof value.text !== "string" ||
    typeof value.model !== "string" ||
    typeof value.finish_reason !== "string" ||
    !isNonNegativeSafeInteger(value.prompt_tokens) ||
    !isNonNegativeSafeInteger(value.completion_tokens)
  ) {
    throw new TypeError(
      "The Rust model completion bridge returned an invalid result.",
    );
  }
}

function resolveHost() {
  const testHost = globalThis.__GEO_AGENT_TEST_HOST__;
  if (isHost(testHost)) {
    return testHost;
  }

  const denoOps = globalThis.Deno?.core?.ops;
  if (
    !denoOps ||
    typeof denoOps.op_host_model_complete_v1 !== "function" ||
    typeof denoOps.op_host_emit !== "function"
  ) {
    throw new Error("The approved Rust host-op surface is not available.");
  }

  return {
    async emit(topic, payload) {
      await denoOps.op_host_emit(topic, payload);
    },
    async modelComplete(request) {
      return JSON.parse(
        await denoOps.op_host_model_complete_v1(
          JSON.stringify(withoutUndefined(request)),
        ),
      );
    },
  };
}

function isHost(value) {
  return (
    isRecord(value) &&
    typeof value.emit === "function" &&
    typeof value.modelComplete === "function"
  );
}

function withoutUndefined(value) {
  return Object.fromEntries(
    Object.entries(value).filter(([, entry]) => entry !== undefined),
  );
}

function getSession(conversationId) {
  let session = sessions.get(conversationId);
  if (!session) {
    session = createSession(conversationId);
    sessions.set(conversationId, session);
  }
  return session;
}

function createSession(conversationId) {
  const events = [];
  const messages = [];
  let nextSequence = 0;

  const storage = {
    async appendLocalEvent(draft) {
      const originSequence = ++nextSequence;
      const event = {
        ...draft,
        lamportClock: originSequence,
        originSequence,
      };
      events.push(event);
      if (event.kind === "message") {
        messages.push({
          ...event.message,
          conversationId: event.conversationId,
          lamportClock: event.lamportClock,
          originNodeId: event.originNodeId,
          originSequence: event.originSequence,
          timestamp: event.timestamp,
        });
      }
      return event;
    },
    async getAgentDefinition(definitionId) {
      return definitionId === "geo-embedded-agent"
        ? createDefinition(DEFAULT_MODEL_ID)
        : null;
    },
    async getCompactionCandidatePage() {
      return {
        hasMore: false,
        messages: [],
        newlyCoveredMessageCountByOrigin: {},
        newlyCoveredUserTurnCountByOrigin: {},
        nextCoveredVersion: {},
      };
    },
    async getConversationMeta(id) {
      return id === conversationId
        ? { conversationId, definitionId: "geo-embedded-agent" }
        : null;
    },
    async getFullContentMessagePage(id) {
      if (id !== conversationId) {
        throw new Error(
          "The embedded conversation store rejected a foreign conversation.",
        );
      }
      return {
        conversationId,
        hasMoreAfter: false,
        hasMoreBefore: false,
        items: [...messages],
        reset: false,
        revision: String(events.length),
      };
    },
    async getRetainedCompactionControls() {
      return {
        hasMore: false,
        invalidated: false,
        items: [],
      };
    },
  };

  return {
    latestAssistant(turnId) {
      return [...messages]
        .reverse()
        .find(
          (message) =>
            message.role === "assistant" && message.turnId === turnId,
        );
    },
    storage,
  };
}

const EMPTY_TOOL_REGISTRY = {
  getTool: () => undefined,
  listTools: () => [],
  registerTool: () => {
    throw new Error("The embedded loop has no direct JavaScript tools.");
  },
};

const NOOP_LIFECYCLE = {
  async start() {},
  async stop() {},
};

function requiredString(value, field) {
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new TypeError(`${field} must be a non-empty string.`);
  }
  return value;
}

function requiredPositiveSafeInteger(value, field) {
  if (!isNonNegativeSafeInteger(value)) {
    throw new TypeError(`${field} must be a non-negative safe integer.`);
  }
  return value;
}

function isNonNegativeSafeInteger(value) {
  return Number.isSafeInteger(value) && value >= 0;
}

function isRecord(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
