import { createAgentToolLoopRunner } from "memeloop/loop-api";

const DEFAULT_MODEL_ID = "geo-default";
const DEFAULT_PROVIDER_ID = "geo-host";
const LOCAL_NODE_ID = "geo-embedded-worker";
// OpenAI-compatible function names do not permit dots; the host capability
// itself remains the versioned knowledge.search.v1 op.
const KNOWLEDGE_SEARCH = "knowledge_search";
const KNOWLEDGE_IMPORT_ATTACHMENTS = "knowledge_import_attachments";
const REPORT_GET = "report_get";
const REPORT_REDUCE = "report_reduce";
const REPORT_GET_SCHEMA = {
  type: "object",
  additionalProperties: false,
  properties: {
    report_id: {
      type: "string",
      format: "uuid",
      description: "Omit to read the latest report in the current project.",
    },
  },
};
const REPORT_REDUCE_SCHEMA = {
  type: "object",
  additionalProperties: false,
  properties: {
    cycle_id: {
      type: "string",
      format: "uuid",
      description: "Omit to reduce the current project cycle after its cutoff.",
    },
    correction_of: { type: "string", format: "uuid" },
  },
};
const KNOWLEDGE_SEARCH_SCHEMA = {
  type: "object",
  additionalProperties: false,
  required: ["query"],
  properties: {
    query: { type: "string", minLength: 1 },
    purpose: { type: "string", enum: ["public", "internal"] },
    limit: { type: "integer", minimum: 1, maximum: 50 },
    knowledge_release_id: { type: "string", format: "uuid" },
  },
};
const sessions = new Map();

// deno_core's bare V8 context does not install the browser structuredClone
// global. The pinned MemeLoop loop clones only its detached JSON tool payloads
// and framework state on this path; reject non-JSON data instead of silently
// sharing mutable objects across the loop's snapshots.
if (typeof globalThis.structuredClone !== "function") {
  globalThis.structuredClone = (value) => {
    const json = JSON.stringify(value);
    if (typeof json !== "string") {
      throw new TypeError("The embedded loop can clone only JSON values.");
    }
    return JSON.parse(json);
  };
}

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
  const host = resolveHost(turn.attachments.length > 0);
  const session = getSession(turn.conversationId);
  const modelId = turn.model ?? DEFAULT_MODEL_ID;
  const toolFailures = [];
  const provider = createHostProvider(host, modelId, toolFailures);
  const toolNames =
    turn.attachments.length > 0
      ? [
          KNOWLEDGE_IMPORT_ATTACHMENTS,
          KNOWLEDGE_SEARCH,
          REPORT_GET,
          REPORT_REDUCE,
        ]
      : [KNOWLEDGE_SEARCH, REPORT_GET, REPORT_REDUCE];
  const definition = createDefinition(modelId, toolNames);
  const context = createContext({
    session,
    definition,
    provider,
    modelId,
    tools: createHostTools(host, toolFailures, turn.attachments),
  });
  const runner = createAgentToolLoopRunner(context);

  let exhausted = false;
  for await (const step of runner({
    conversationId: turn.conversationId,
    message: turn.prompt,
    runId: turn.runId,
    userMessage: {
      content: turn.prompt,
      messageId: turn.messageId,
      // The upstream canonical user turn is keyed by messageId. GEO retains
      // its separate persisted turnId for the enclosing Run and completion.
      ...(turn.attachments.length > 0
        ? {
            metadata: {
              attachmentReferences: turn.attachments.map((attachment) =>
                withoutUndefined({
                  attachmentId: attachment.attachmentId,
                  filename: attachment.filename,
                  mimeType: attachment.mimeType,
                  size: attachment.size,
                  contentHash: attachment.contentHash,
                }),
              ),
            },
          }
        : {}),
      timestamp: turn.timestamp,
    },
  })) {
    // The direct runner is intentionally consumed to completion. It persists
    // canonical user/assistant messages in the protocol adapter below.
    if (step.type === "thinking" && step.data?.status === "max-iterations") {
      exhausted = true;
    }
  }

  if (toolFailures.length > 0) {
    throw toolFailures[0];
  }
  if (exhausted) {
    throw new Error("MemeLoop exhausted its model-to-tool iteration budget.");
  }
  const assistant = session.latestAssistant(turn.messageId);
  if (
    !assistant ||
    assistant.toolCalls?.length > 0 ||
    typeof assistant.content !== "string" ||
    assistant.content.trim().length === 0
  ) {
    throw new Error("MemeLoop completed without a final assistant answer.");
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
  if (typeof input.prompt !== "string") {
    throw new TypeError("prompt must be a string.");
  }
  const prompt = input.prompt;
  const turnId = requiredString(input.turn_id, "turn_id");
  const messageId =
    input.message_id === undefined
      ? turnId
      : requiredString(input.message_id, "message_id");
  const runId = requiredString(input.run_id, "run_id");
  const attachments = normalizeAttachments(input.attachments ?? []);
  if (prompt.trim().length === 0 && attachments.length === 0) {
    throw new TypeError("A turn requires a prompt or an attachment.");
  }
  const model =
    input.model === undefined
      ? undefined
      : requiredString(input.model, "model");
  const timestamp =
    input.timestamp === undefined
      ? Date.now()
      : requiredPositiveSafeInteger(input.timestamp, "timestamp");

  return {
    attachments,
    conversationId,
    messageId,
    model,
    prompt,
    runId,
    timestamp,
    turnId,
  };
}

function normalizeAttachments(value) {
  if (!Array.isArray(value)) {
    throw new TypeError("attachments must be an array.");
  }
  const ids = new Set();
  return value.map((attachment) => {
    if (!isRecord(attachment)) {
      throw new TypeError("Each attachment must be an object.");
    }
    const attachmentId = requiredString(
      attachment.attachment_id,
      "attachment_id",
    );
    if (ids.has(attachmentId)) {
      throw new TypeError("Duplicate attachment_id in turn input.");
    }
    ids.add(attachmentId);
    const filename = requiredString(attachment.filename, "filename");
    const mimeType =
      attachment.media_type == null
        ? "application/octet-stream"
        : requiredString(attachment.media_type, "media_type");
    const size =
      attachment.size_bytes == null
        ? undefined
        : requiredPositiveSafeInteger(attachment.size_bytes, "size_bytes");
    const contentHash =
      attachment.sha256 == null
        ? undefined
        : `sha256:${requiredString(attachment.sha256, "sha256").replace(/^sha256:/u, "")}`;
    return { attachmentId, filename, mimeType, size, contentHash };
  });
}

function createContext({ session, definition, modelId, provider, tools }) {
  return {
    agentToolLoop: {
      enableToolLoop: true,
      maxIterations: 4,
      textToolCallProtocolEnabled: false,
      toolPermissions: {
        default: "deny",
        rules: [
          { pattern: KNOWLEDGE_SEARCH, action: "allow" },
          { pattern: KNOWLEDGE_IMPORT_ATTACHMENTS, action: "allow" },
          { pattern: REPORT_GET, action: "allow" },
          { pattern: REPORT_REDUCE, action: "allow" },
        ],
      },
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
    tools,
  };
}

function createDefinition(modelId, toolNames = [KNOWLEDGE_SEARCH]) {
  return {
    description: "Rust-hosted MemeLoop agent turn",
    id: "geo-embedded-agent",
    modelConfig: {
      modelId,
      providerId: DEFAULT_PROVIDER_ID,
    },
    name: "GEO embedded agent",
    systemPrompt: "",
    tools: toolNames,
    version: "1",
  };
}

function createHostProvider(host, configuredModel, toolFailures) {
  const provider = {
    lastModel: undefined,
    modelId: configuredModel,
    name: "geo-host-bridge",
    async *chat(request) {
      if (toolFailures.length > 0) {
        throw toolFailures[0];
      }
      const response = await host.modelComplete({
        max_output_tokens: request.maxOutputTokens,
        model:
          configuredModel === DEFAULT_MODEL_ID ? undefined : configuredModel,
        prompt: promptFromMessages(request.messages),
        system: systemFromMessages(request.messages),
        messages: request.messages.map(toHostMessage),
        tools: (request.tools ?? []).map((tool) => ({
          type: "function",
          function: {
            name: tool.name,
            description: tool.description ?? "",
            parameters: tool.inputSchema,
          },
        })),
      });
      assertModelCompletion(
        response,
        new Set((request.tools ?? []).map((tool) => tool.name)),
      );
      provider.lastModel = response.model;

      if (response.text.length > 0) {
        yield {
          id: "geo-host-completion",
          text: response.text,
          type: "text-delta",
        };
      }
      for (const call of response.tool_calls ?? []) {
        yield {
          type: "tool-call",
          toolCallId: call.id,
          toolName: call.function.name,
          input: JSON.parse(call.function.arguments),
        };
      }
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

function toHostMessage(message) {
  if (message.role === "tool") {
    if (message.content.length !== 1) {
      throw new TypeError(
        "The host model bridge requires one tool result per message.",
      );
    }
    const result = message.content[0];
    return {
      role: "tool",
      tool_call_id: result.toolCallId,
      content: result.output.type.endsWith("json")
        ? JSON.stringify(result.output.value)
        : result.output.value,
    };
  }
  if (message.role !== "assistant" || typeof message.content === "string") {
    return {
      role: message.role,
      content: messageContent(message.content),
    };
  }
  const calls = message.content.filter((part) => part.type === "tool-call");
  return {
    role: "assistant",
    content:
      message.content
        .filter((part) => part.type === "text" || part.type === "reasoning")
        .map((part) => part.text)
        .join("\n") || null,
    ...(calls.length > 0
      ? {
          tool_calls: calls.map((call) => ({
            id: call.toolCallId,
            type: "function",
            function: {
              name: call.toolName,
              arguments: JSON.stringify(call.input),
            },
          })),
        }
      : {}),
  };
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

function assertModelCompletion(value, allowedTools) {
  if (
    !isRecord(value) ||
    typeof value.text !== "string" ||
    typeof value.model !== "string" ||
    typeof value.finish_reason !== "string" ||
    !isNonNegativeSafeInteger(value.prompt_tokens) ||
    !isNonNegativeSafeInteger(value.completion_tokens) ||
    (value.tool_calls !== undefined &&
      (!Array.isArray(value.tool_calls) ||
        value.tool_calls.some(
          (call) =>
            !isRecord(call) ||
            typeof call.id !== "string" ||
            call.type !== "function" ||
            !isRecord(call.function) ||
            !allowedTools.has(call.function.name) ||
            typeof call.function.arguments !== "string",
        )))
  ) {
    throw new TypeError(
      "The Rust model completion bridge returned an invalid result.",
    );
  }
}

function resolveHost(requireImport) {
  const testHost = globalThis.__GEO_AGENT_TEST_HOST__;
  if (
    isHost(testHost) &&
    (!requireImport ||
      typeof testHost.knowledgeImportAttachments === "function")
  ) {
    return testHost;
  }

  const denoOps = globalThis.Deno?.core?.ops;
  if (
    !denoOps ||
    typeof denoOps.op_host_model_complete_v1 !== "function" ||
    typeof denoOps.op_host_knowledge_search_v1 !== "function" ||
    typeof denoOps.op_host_report_get_v1 !== "function" ||
    typeof denoOps.op_host_report_reduce_v1 !== "function" ||
    (requireImport &&
      typeof denoOps.op_host_knowledge_import_attachments_v1 !== "function") ||
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
    async knowledgeSearch(request) {
      return JSON.parse(
        await denoOps.op_host_knowledge_search_v1(JSON.stringify(request)),
      );
    },
    async knowledgeImportAttachments(request) {
      return JSON.parse(
        await denoOps.op_host_knowledge_import_attachments_v1(
          JSON.stringify(request),
        ),
      );
    },
    async reportGet(request) {
      return JSON.parse(
        await denoOps.op_host_report_get_v1(JSON.stringify(request)),
      );
    },
    async reportReduce(request) {
      return JSON.parse(
        await denoOps.op_host_report_reduce_v1(JSON.stringify(request)),
      );
    },
  };
}

function isHost(value) {
  return (
    isRecord(value) &&
    typeof value.emit === "function" &&
    typeof value.modelComplete === "function" &&
    typeof value.knowledgeSearch === "function"
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

function createHostTools(host, failures, attachments) {
  const search = async (parameters) => {
    try {
      if (!isRecord(parameters)) {
        throw new TypeError("knowledge.search requires an object.");
      }
      return { result: await host.knowledgeSearch(parameters) };
    } catch (error) {
      failures.push(error);
      throw error;
    }
  };
  const importAttachments = async (parameters) => {
    try {
      if (!isRecord(parameters) || !Array.isArray(parameters.items)) {
        throw new TypeError("knowledge.import_attachments requires items.");
      }
      return { result: await host.knowledgeImportAttachments(parameters) };
    } catch (error) {
      failures.push(error);
      throw error;
    }
  };
  const reportTool = (name, method) => async (parameters) => {
    try {
      if (!isRecord(parameters)) {
        throw new TypeError(`${name} requires an object.`);
      }
      return { result: await host[method](parameters) };
    } catch (error) {
      failures.push(error);
      throw error;
    }
  };
  const reportGet = reportTool(REPORT_GET, "reportGet");
  const reportReduce = reportTool(REPORT_REDUCE, "reportReduce");
  const importSchema = {
    type: "object",
    additionalProperties: false,
    required: ["items"],
    properties: {
      items: {
        type: "array",
        minItems: 1,
        maxItems: attachments.length,
        items: {
          type: "object",
          additionalProperties: false,
          required: ["attachment_id", "purpose"],
          properties: {
            attachment_id: {
              type: "string",
              enum: attachments.map(({ attachmentId }) => attachmentId),
              description: `Current message attachments: ${attachments
                .map(
                  ({ attachmentId, filename, mimeType }) =>
                    `${attachmentId} (${filename}, ${mimeType})`,
                )
                .join("; ")}`,
            },
            purpose: { type: "string", enum: ["public", "internal"] },
          },
        },
      },
    },
  };
  return {
    getTool: (id) =>
      id === KNOWLEDGE_SEARCH
        ? search
        : id === KNOWLEDGE_IMPORT_ATTACHMENTS && attachments.length > 0
          ? importAttachments
          : id === REPORT_GET
            ? reportGet
            : id === REPORT_REDUCE
              ? reportReduce
              : undefined,
    listTools: () =>
      attachments.length > 0
        ? [
            KNOWLEDGE_IMPORT_ATTACHMENTS,
            KNOWLEDGE_SEARCH,
            REPORT_GET,
            REPORT_REDUCE,
          ]
        : [KNOWLEDGE_SEARCH, REPORT_GET, REPORT_REDUCE],
    getToolParameterSchema: (id) =>
      id === KNOWLEDGE_SEARCH
        ? KNOWLEDGE_SEARCH_SCHEMA
        : id === KNOWLEDGE_IMPORT_ATTACHMENTS && attachments.length > 0
          ? importSchema
          : id === REPORT_GET
            ? REPORT_GET_SCHEMA
            : id === REPORT_REDUCE
              ? REPORT_REDUCE_SCHEMA
              : undefined,
    registerTool: () => {
      throw new Error("The embedded loop cannot register tools.");
    },
  };
}

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
