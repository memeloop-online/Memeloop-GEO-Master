import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import {
  Button,
  MessageBar,
  MessageBarBody,
  Spinner,
} from "@fluentui/react-components";
import {
  AddRegular,
  ArrowSyncRegular,
  DismissRegular,
  FolderOpenRegular,
} from "@fluentui/react-icons";
import { AgentChatView } from "@memeloop/react-ui/agent";
import type { WebMemeLoopChatAdapter } from "@memeloop/react-ui/chat";
import { createTheme, ThemeProvider } from "@mui/material/styles";
import type { ConversationMessageListProjection } from "memeloop";
import { useNavigate, useParams } from "react-router-dom";
import {
  agentEventStreamUrl,
  cancelAgentTurn,
  openAgentEventStream,
  postAgentMessage,
  type AgentConversationDetail,
  type AgentConversationSummary,
  type AgentMessage,
  type AgentRun,
  useAgentConversationQuery,
  useAgentConversationsQuery,
  useCreateAgentConversationMutation,
} from "../api/agent";
import { ErrorState, EmptyState, LoadingState } from "../components/AsyncState";

const agentTheme = createTheme({
  palette: {
    primary: { main: "#0f6cbd" },
    background: { default: "#ffffff", paper: "#ffffff" },
  },
  typography: {
    fontFamily: '"Segoe UI", "Microsoft YaHei UI", system-ui, sans-serif',
  },
});

function projectMessage(
  message: AgentMessage,
): ConversationMessageListProjection {
  const parsedTimestamp = Date.parse(message.created_at);
  const metadata =
    typeof message.metadata === "object" &&
    message.metadata !== null &&
    !Array.isArray(message.metadata)
      ? (message.metadata as Record<string, unknown>)
      : undefined;
  return {
    messageId: message.id,
    // The API's system messages have no turn. The message identity keeps the
    // rendering projection stable without inventing a server-side turn.
    turnId: message.turn_id ?? message.id,
    conversationId: message.conversation_id,
    originNodeId: "geo-api",
    originSequence: message.sequence,
    timestamp: Number.isFinite(parsedTimestamp) ? parsedTimestamp : 0,
    lamportClock: message.sequence,
    role: message.role === "system" ? "agent" : message.role,
    content: message.content,
    metadata,
  };
}

function conversationName(
  conversation: Pick<AgentConversationSummary, "title" | "created_at">,
) {
  if (conversation.title?.trim()) return conversation.title;
  const date = new Date(conversation.created_at);
  if (Number.isNaN(date.getTime())) return "未命名对话";
  return `新对话 · ${date.toLocaleDateString("zh-CN")}`;
}

function orderedRuns(runs: readonly AgentRun[]) {
  return [...runs].sort((left, right) =>
    right.updated_at.localeCompare(left.updated_at),
  );
}

function unavailableRuntimeNotice(runs: readonly AgentRun[]) {
  const failedRun = orderedRuns(runs).find(
    (run) =>
      run.status === "failed" &&
      ["missing", "unavailable"].includes(run.capability.status),
  );
  if (!failedRun) return undefined;
  const runtime =
    failedRun.capability.runtime === "deno_core"
      ? "Rust JS Agent Runtime"
      : failedRun.capability.runtime;
  return failedRun.capability.status === "missing"
    ? `${runtime} 尚未配置，本次未生成 AI 回复。`
    : `${runtime} 当前不可用，本次未生成 AI 回复。`;
}

function ConversationList({
  activeConversationId,
  items,
  pending,
  error,
  creating,
  onCreate,
  onSelect,
  onRetry,
}: {
  activeConversationId?: string;
  items: Array<Pick<AgentConversationSummary, "id" | "title" | "created_at">>;
  pending: boolean;
  error: boolean;
  creating: boolean;
  onCreate: () => void;
  onSelect: (conversationId: string) => void;
  onRetry: () => void;
}) {
  return (
    <aside className="agent-conversation-list" aria-label="AI 对话列表">
      <div className="agent-conversation-list-heading">
        <div>
          <p className="eyebrow">P00</p>
          <h2>AI 工作台</h2>
        </div>
        <Button
          appearance="primary"
          size="small"
          icon={<AddRegular />}
          disabled={creating}
          onClick={onCreate}
        >
          {creating ? "正在创建…" : "新建"}
        </Button>
      </div>
      <p className="agent-conversation-list-description">
        对话和运行记录按当前项目隔离。
      </p>
      <div className="agent-conversation-items">
        {pending && <LoadingState compact label="正在加载对话" />}
        {error && (
          <Button
            appearance="subtle"
            icon={<ArrowSyncRegular />}
            onClick={onRetry}
          >
            重新加载对话
          </Button>
        )}
        {!pending && !error && items.length === 0 && (
          <p className="agent-conversation-list-empty">还没有对话。</p>
        )}
        {items.map((conversation) => (
          <Button
            key={conversation.id}
            appearance={
              conversation.id === activeConversationId ? "primary" : "subtle"
            }
            className="agent-conversation-item"
            onClick={() => onSelect(conversation.id)}
          >
            {conversationName(conversation)}
          </Button>
        ))}
      </div>
    </aside>
  );
}

function AgentChat({
  conversation,
  tenantId,
  projectId,
  onRefresh,
}: {
  conversation: AgentConversationDetail;
  tenantId: string;
  projectId: string;
  onRefresh: () => Promise<void>;
}) {
  const [selectedFile, setSelectedFile] = useState<File>();
  const [localTurnId, setLocalTurnId] = useState<string>();
  const [runtimeNotice, setRuntimeNotice] = useState<string>();
  const activeRun = orderedRuns(conversation.runs).find((run) =>
    ["queued", "running"].includes(run.status),
  );
  const activeTurnId = activeRun?.turn_id ?? localTurnId;
  const capabilityNotice = unavailableRuntimeNotice(conversation.runs);
  const displayedRuntimeNotice = capabilityNotice ?? runtimeNotice;

  const adapter = useMemo<WebMemeLoopChatAdapter>(
    () => ({
      conversationId: conversation.conversation.id,
      messages: conversation.messages.map(projectMessage),
      isRunning: Boolean(activeTurnId),
      isLoading: false,
      error: null,
      sendMessage: async ({ text, file }) => {
        const content = text.trim();
        if (!content) return;
        if (file || selectedFile) {
          throw new Error("agent-upload-reference-required");
        }
        setRuntimeNotice(undefined);
        const acceptance = await postAgentMessage(
          tenantId,
          projectId,
          conversation.conversation.id,
          { content, attachments: [] },
        );
        setLocalTurnId(
          ["queued", "running"].includes(acceptance.run_status)
            ? acceptance.turn_id
            : undefined,
        );
        if (acceptance.error?.code === "capability_missing") {
          setRuntimeNotice(
            "Rust JS Agent Runtime 尚未配置，本次未生成 AI 回复。",
          );
        }
        await onRefresh();
      },
      cancel: async () => {
        if (!activeTurnId) return;
        setRuntimeNotice(undefined);
        await cancelAgentTurn(tenantId, projectId, activeTurnId);
        setLocalTurnId(undefined);
        await onRefresh();
      },
      deleteTurn: async () => {
        throw new Error("agent-turn-delete-unavailable");
      },
      retryTurn: async () => {
        throw new Error("agent-turn-retry-unavailable");
      },
      onError: () => {
        setRuntimeNotice(
          "操作未完成。请确认 Agent 运行时已配置且当前项目有权限后重试。",
        );
      },
    }),
    [
      activeTurnId,
      conversation.conversation.id,
      conversation.messages,
      onRefresh,
      projectId,
      selectedFile,
      tenantId,
    ],
  );

  const empty: ReactNode = (
    <div className="agent-chat-empty">
      <FolderOpenRegular fontSize={28} aria-hidden="true" />
      <h2>此对话还没有消息</h2>
      <p>输入任务后，服务端会受理并创建可追踪的 Agent 运行。</p>
    </div>
  );

  return (
    <section className="agent-chat-column" aria-label="AI 对话">
      <div className="agent-chat-titlebar">
        <div>
          <p className="eyebrow">P00 · AI 工作台</p>
          <h1>{conversationName(conversation.conversation)}</h1>
        </div>
        {activeTurnId && (
          <span className="agent-run-state" aria-live="polite">
            正在运行
          </span>
        )}
      </div>
      {selectedFile && (
        <MessageBar intent="warning" className="agent-file-reference-notice">
          <MessageBarBody>
            已选择“{selectedFile.name}
            ”。当前聊天接口只接受已上传对象的附件引用，
            文件不会作为浏览器原始内容发送。
          </MessageBarBody>
          <Button
            appearance="subtle"
            icon={<DismissRegular />}
            aria-label={`移除文件 ${selectedFile.name}`}
            onClick={() => setSelectedFile(undefined)}
          />
        </MessageBar>
      )}
      {displayedRuntimeNotice && (
        <MessageBar intent="warning" className="agent-runtime-notice">
          <MessageBarBody>{displayedRuntimeNotice}</MessageBarBody>
        </MessageBar>
      )}
      <div className="agent-chat-surface">
        <ThemeProvider theme={agentTheme}>
          <AgentChatView
            adapter={adapter}
            empty={empty}
            selectedFile={selectedFile}
            onFileSelect={setSelectedFile}
            onClearFile={() => setSelectedFile(undefined)}
            placeholder="描述你希望 AI 协助完成的项目任务"
            composerLabels={{
              input: "输入任务",
              send: "发送",
              cancel: "取消运行",
              addFile: "添加文件",
              removeFile: (filename) => `移除文件 ${filename}`,
            }}
            emptyMessage="此对话还没有消息"
            loadingMessage="正在读取对话…"
            genericErrorMessage="Agent 运行时暂不可用，暂时无法读取消息。"
            operationErrorMessage="操作未完成；未生成任何模拟回复。"
            showTurnActions={false}
            showTimeline={false}
          />
        </ThemeProvider>
      </div>
    </section>
  );
}

export function AgentWorkbenchPage() {
  const { tenantId, projectId, conversationId } = useParams();
  const navigate = useNavigate();
  const conversations = useAgentConversationsQuery(tenantId, projectId);
  const conversation = useAgentConversationQuery(
    tenantId,
    projectId,
    conversationId,
  );
  const createConversation = useCreateAgentConversationMutation(
    tenantId,
    projectId,
  );
  const refetchConversations = conversations.refetch;
  const refetchConversation = conversation.refetch;

  const base =
    tenantId && projectId
      ? `/app/${encodeURIComponent(tenantId)}/${encodeURIComponent(projectId)}/chat`
      : "/workspaces";

  const refresh = useCallback(async () => {
    await Promise.all([refetchConversations(), refetchConversation()]);
  }, [refetchConversation, refetchConversations]);

  useEffect(() => {
    if (
      !tenantId ||
      !projectId ||
      !conversationId ||
      typeof EventSource === "undefined"
    ) {
      return;
    }
    const stream = openAgentEventStream(
      agentEventStreamUrl(tenantId, projectId, conversationId),
      () => void refresh(),
    );
    return () => stream.close();
  }, [conversationId, projectId, refresh, tenantId]);

  async function handleCreate() {
    if (!tenantId || !projectId) return;
    const created = await createConversation.mutateAsync({});
    navigate(`${base}/${encodeURIComponent(created.id)}`);
  }

  function selectConversation(nextConversationId: string) {
    navigate(`${base}/${encodeURIComponent(nextConversationId)}`);
  }

  return (
    <div className="agent-workbench-page" data-testid="agent-workbench-page">
      <div className="agent-workbench-layout">
        <ConversationList
          activeConversationId={conversationId}
          items={conversations.data?.items ?? []}
          pending={conversations.isPending}
          error={conversations.isError}
          creating={createConversation.isPending}
          onCreate={() => void handleCreate()}
          onSelect={selectConversation}
          onRetry={() => void conversations.refetch()}
        />
        <div className="agent-workbench-content">
          {conversationId && conversation.isPending && (
            <div className="agent-workbench-loading">
              <Spinner label="正在加载对话" />
            </div>
          )}
          {conversationId && conversation.isError && (
            <ErrorState
              title="AI 运行时暂不可用"
              detail="无法读取当前对话。请检查服务连接或稍后重试。"
              onRetry={() => void conversation.refetch()}
            />
          )}
          {!conversationId && (
            <EmptyState
              title="从一个项目任务开始"
              detail="新建对话后，才会向当前项目的 Agent 运行时提交任务。这里不会生成演示回复。"
              action={
                <Button
                  appearance="primary"
                  icon={<AddRegular />}
                  disabled={createConversation.isPending}
                  onClick={() => void handleCreate()}
                >
                  新建对话
                </Button>
              }
            />
          )}
          {conversation.data && tenantId && projectId && (
            <AgentChat
              key={conversation.data.conversation.id}
              conversation={conversation.data}
              tenantId={tenantId}
              projectId={projectId}
              onRefresh={refresh}
            />
          )}
        </div>
      </div>
    </div>
  );
}
