import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ChangeEvent,
  type ClipboardEvent,
  type DragEvent,
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
import { useTranslation } from "react-i18next";
import {
  agentEventStreamUrl,
  cancelAgentTurn,
  createAgentConversation,
  openAgentEventStream,
  postAgentMessage,
  uploadAgentAttachment,
  type AgentAttachmentReference,
  type AgentAttachmentUploadState,
  type AgentConversationDetail,
  type AgentConversationSummary,
  type AgentMessage,
  type AgentRun,
  useAgentConversationQuery,
  useAgentConversationsQuery,
} from "../api/agent";
import { createIdempotencyKey } from "../api/client";
import { ErrorState, LoadingState } from "../components/AsyncState";
import { useAppearance } from "../appearance/AppearanceProvider";
import { formatUiDate } from "../i18n";

// A composer projection only, never saved or presented as a server conversation.
const emptyConversation: AgentConversationDetail = {
  conversation: {
    id: "local-unsent-composer",
    title: "",
    status: "active",
    revision: 0,
    created_at: "",
    updated_at: "",
  },
  messages: [],
  turns: [],
  runs: [],
};

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
  unnamed: string,
  newConversation: string,
) {
  if (conversation.title?.trim()) return conversation.title;
  if (!conversation.created_at) return newConversation;
  const date = new Date(conversation.created_at);
  if (Number.isNaN(date.getTime())) return unnamed;
  return `${newConversation} · ${formatUiDate(date)}`;
}

function orderedRuns(runs: readonly AgentRun[]) {
  return [...runs].sort((left, right) =>
    right.updated_at.localeCompare(left.updated_at),
  );
}

function terminalRunNotice(
  run: AgentRun | undefined,
  missing: string,
  unavailable: string,
  failed: string,
  cancelled: string,
) {
  if (run?.status === "cancelled") return cancelled;
  if (run?.status !== "failed") return undefined;
  if (run.capability.status === "missing") return missing;
  if (run.capability.status === "unavailable") return unavailable;
  return failed;
}

interface PendingAttachment {
  id: string;
  file: File;
  status: "waiting" | AgentAttachmentUploadState | "failed";
  createKey: string;
  completeKey: string;
  sessionId?: string;
  contentUploaded?: boolean;
  reference?: AgentAttachmentReference;
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
  const { t } = useTranslation();
  return (
    <aside
      className="agent-conversation-list"
      aria-label={t("chatWorkbench.conversationList")}
    >
      <div className="agent-conversation-list-heading">
        <div>
          <h2>{t("chatWorkbench.title")}</h2>
        </div>
        {(activeConversationId || items.length > 0) && (
          <Button
            appearance="primary"
            size="small"
            icon={<AddRegular />}
            disabled={creating}
            onClick={onCreate}
          >
            {creating ? t("chatWorkbench.creating") : t("chatWorkbench.create")}
          </Button>
        )}
      </div>
      <p className="agent-conversation-list-description">
        {t("chatWorkbench.description")}
      </p>
      <div className="agent-conversation-items">
        {pending && <LoadingState compact label={t("chatWorkbench.loading")} />}
        {error && (
          <Button
            appearance="subtle"
            icon={<ArrowSyncRegular />}
            onClick={onRetry}
          >
            {t("chatWorkbench.reload")}
          </Button>
        )}
        {!pending && !error && items.length === 0 && (
          <p className="agent-conversation-list-empty">
            {t("chatWorkbench.noConversations")}
          </p>
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
            {conversationName(
              conversation,
              t("chatWorkbench.unnamedConversation"),
              t("chatWorkbench.newConversation"),
            )}
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
  onFirstMessage,
}: {
  conversation: AgentConversationDetail;
  tenantId: string;
  projectId: string;
  onRefresh: () => Promise<void>;
  onFirstMessage?: (conversationId: string) => void;
}) {
  const { t } = useTranslation();
  const { appearance } = useAppearance();
  const agentTheme = useMemo(
    () =>
      createTheme({
        palette: {
          primary: { main: appearance.primary_color },
          background: { default: "#ffffff", paper: "#ffffff" },
        },
        typography: {
          fontFamily: '"Segoe UI", "Microsoft YaHei UI", system-ui, sans-serif',
        },
      }),
    [appearance.primary_color],
  );
  const [attachments, setAttachments] = useState<PendingAttachment[]>([]);
  const [attachmentNotice, setAttachmentNotice] = useState<string>();
  const [localTurnId, setLocalTurnId] = useState<string>();
  const [runtimeNotice, setRuntimeNotice] = useState<string>();
  const picker = useRef<HTMLInputElement>(null);
  const submissionInFlight = useRef(false);
  const pendingConversation = useRef<{ key: string; id?: string } | undefined>(
    undefined,
  );
  const [submitting, setSubmitting] = useState(false);
  const pendingSubmission = useRef<
    { signature: string; key: string } | undefined
  >(undefined);
  const selectedFile = attachments[0]?.file;
  const latestRun = orderedRuns(conversation.runs)[0];
  const activeRun =
    latestRun && ["queued", "running"].includes(latestRun.status)
      ? latestRun
      : undefined;
  const localTurnIsTerminal = conversation.runs.some(
    (run) =>
      run.turn_id === localTurnId &&
      ["succeeded", "failed", "cancelled"].includes(run.status),
  );
  const activeTurnId =
    activeRun?.turn_id ?? (localTurnIsTerminal ? undefined : localTurnId);
  const persistedNotice = terminalRunNotice(
    latestRun,
    t("chatWorkbench.runtimeMissing"),
    t("chatWorkbench.runtimeUnavailable"),
    t("chatWorkbench.runFailed"),
    t("chatWorkbench.runCancelled"),
  );
  const displayedRuntimeNotice =
    persistedNotice ?? (runtimeNotice ? t(runtimeNotice) : undefined);
  useEffect(() => {
    if (localTurnId && localTurnIsTerminal) setLocalTurnId(undefined);
  }, [localTurnId, localTurnIsTerminal]);
  const latestAnswer = [...conversation.messages]
    .filter((message) => message.role === "assistant")
    .sort((a, b) => b.sequence - a.sequence)[0];
  const omittedHistory =
    latestAnswer?.metadata &&
    typeof latestAnswer.metadata === "object" &&
    !Array.isArray(latestAnswer.metadata)
      ? (latestAnswer.metadata as Record<string, unknown>).history_omitted_turns
      : undefined;

  function addFiles(files: readonly File[]) {
    if (!files.length) return;
    setAttachments((current) => {
      const available = Math.max(0, 100 - current.length);
      const accepted = files.slice(0, available).filter((file) => {
        if (file.size > 100 * 1024 * 1024) {
          setAttachmentNotice("chatWorkbench.fileTooLarge");
          return false;
        }
        return true;
      });
      if (files.length > available) {
        setAttachmentNotice("chatWorkbench.tooManyFiles");
      }
      return [
        ...current,
        ...accepted.map((file) => ({
          id: createIdempotencyKey(),
          file,
          status: "waiting" as const,
          createKey: createIdempotencyKey(),
          completeKey: createIdempotencyKey(),
        })),
      ];
    });
  }

  function updateAttachment(id: string, update: Partial<PendingAttachment>) {
    setAttachments((current) =>
      current.map((item) => (item.id === id ? { ...item, ...update } : item)),
    );
  }

  async function upload(item: PendingAttachment) {
    if (item.reference) return item.reference;
    try {
      const reference = await uploadAgentAttachment(
        tenantId,
        projectId,
        item.file,
        {
          createKey: item.createKey,
          completeKey: item.completeKey,
          sessionId: item.sessionId,
          contentUploaded: item.contentUploaded,
          onProgress: (status, sessionId) =>
            updateAttachment(item.id, {
              status,
              sessionId,
              contentUploaded:
                item.contentUploaded ||
                status === "completing" ||
                status === "uploaded",
            }),
        },
      );
      updateAttachment(item.id, { status: "uploaded", reference });
      return reference;
    } catch (error) {
      updateAttachment(item.id, { status: "failed" });
      throw error;
    }
  }

  function onPickerChange(event: ChangeEvent<HTMLInputElement>) {
    addFiles(Array.from(event.target.files ?? []));
    event.target.value = "";
  }

  function onDropCapture(event: DragEvent<HTMLElement>) {
    if (!event.dataTransfer.files.length) return;
    event.preventDefault();
    event.stopPropagation();
    addFiles(Array.from(event.dataTransfer.files));
  }

  function onPasteCapture(event: ClipboardEvent<HTMLElement>) {
    if (!event.clipboardData.files.length) return;
    event.preventDefault();
    event.stopPropagation();
    addFiles(Array.from(event.clipboardData.files));
  }

  const adapter = useMemo<WebMemeLoopChatAdapter>(
    () => ({
      conversationId: conversation.conversation.id,
      messages: conversation.messages.map(projectMessage),
      isRunning: Boolean(activeTurnId),
      isLoading: false,
      error:
        latestRun?.status === "failed" &&
        latestRun.capability.status === "available"
          ? new Error(t("chatWorkbench.runFailed"))
          : null,
      sendMessage: async ({ text, file }) => {
        if (submissionInFlight.current) return;
        const content = text.trim();
        const batch = attachments.length
          ? attachments
          : file
            ? [
                {
                  id: createIdempotencyKey(),
                  file,
                  status: "waiting" as const,
                  createKey: createIdempotencyKey(),
                  completeKey: createIdempotencyKey(),
                },
              ]
            : [];
        if (!content && !batch.length) return;
        submissionInFlight.current = true;
        setSubmitting(true);
        try {
          setAttachmentNotice(undefined);
          const results = await Promise.allSettled(batch.map(upload));
          if (results.some((result) => result.status === "rejected")) {
            setAttachmentNotice("chatWorkbench.partialUploadFailed");
            throw new Error("agent-attachment-upload-incomplete");
          }
          const references = results.map(
            (result) =>
              (result as PromiseFulfilledResult<AgentAttachmentReference>)
                .value,
          );
          const signature = JSON.stringify({
            content,
            attachmentIds: references.map(
              (reference) => reference.attachment_id,
            ),
          });
          if (pendingSubmission.current?.signature !== signature) {
            pendingSubmission.current = {
              signature,
              key: createIdempotencyKey(),
            };
          }
          setRuntimeNotice(undefined);
          let targetConversationId = conversation.conversation.id;
          if (onFirstMessage) {
            pendingConversation.current ??= { key: createIdempotencyKey() };
            if (!pendingConversation.current.id) {
              const created = await createAgentConversation(
                tenantId,
                projectId,
                {},
                pendingConversation.current.key,
              );
              pendingConversation.current.id = created.id;
            }
            targetConversationId = pendingConversation.current.id;
          }
          const acceptance = await postAgentMessage(
            tenantId,
            projectId,
            targetConversationId,
            { content, attachments: references },
            pendingSubmission.current.key,
          );
          setLocalTurnId(
            ["queued", "running"].includes(acceptance.run_status)
              ? acceptance.turn_id
              : undefined,
          );
          if (references.length) {
            setRuntimeNotice("chatWorkbench.uploadedHint");
          }
          if (acceptance.error?.code === "capability_missing") {
            setRuntimeNotice("chatWorkbench.runtimeMissing");
          }
          if (!onFirstMessage) await onRefresh();
          pendingSubmission.current = undefined;
          setAttachments([]);
          onFirstMessage?.(targetConversationId);
        } finally {
          submissionInFlight.current = false;
          setSubmitting(false);
        }
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
        if (attachmentNotice) return;
        setRuntimeNotice("chatWorkbench.operationFailed");
      },
    }),
    [
      activeTurnId,
      conversation.conversation.id,
      conversation.messages,
      latestRun?.status,
      latestRun?.capability.status,
      onRefresh,
      onFirstMessage,
      projectId,
      attachments,
      attachmentNotice,
      tenantId,
      t,
    ],
  );

  const empty: ReactNode = (
    <div className="agent-chat-empty">
      <FolderOpenRegular fontSize={28} aria-hidden="true" />
      <h2>{t("chatWorkbench.emptyTitle")}</h2>
      <p>{t("chatWorkbench.emptyHint")}</p>
      <p>{t("chatWorkbench.emptyDetail")}</p>
    </div>
  );

  return (
    <section
      className="agent-chat-column"
      aria-label={t("chatWorkbench.conversation")}
      onDragOverCapture={(event) => {
        if (event.dataTransfer.types.includes("Files")) event.preventDefault();
      }}
      onDropCapture={onDropCapture}
      onPasteCapture={onPasteCapture}
    >
      <div className="agent-chat-titlebar">
        <div>
          <p className="eyebrow">{t("chatWorkbench.title")}</p>
          <h1>
            {conversationName(
              conversation.conversation,
              t("chatWorkbench.unnamedConversation"),
              t("chatWorkbench.newConversation"),
            )}
          </h1>
        </div>
        {activeTurnId && (
          <span className="agent-run-state" aria-live="polite">
            {t(
              activeRun?.status === "queued"
                ? "chatWorkbench.queued"
                : "chatWorkbench.running",
            )}
          </span>
        )}
      </div>
      {attachmentNotice && (
        <MessageBar intent="warning" className="agent-file-reference-notice">
          <MessageBarBody>{t(attachmentNotice)}</MessageBarBody>
        </MessageBar>
      )}
      {typeof omittedHistory === "number" &&
        Number.isSafeInteger(omittedHistory) &&
        omittedHistory > 0 && (
          <MessageBar
            intent="info"
            aria-label={t("chatWorkbench.historyScope")}
          >
            <MessageBarBody>
              {t("chatWorkbench.historyOmitted", { count: omittedHistory })}
            </MessageBarBody>
          </MessageBar>
        )}
      {attachments.length > 0 && (
        <div
          className="agent-file-reference-notice"
          aria-label={t("chatWorkbench.pendingAttachments")}
        >
          <p>{t("chatWorkbench.attachmentHint")}</p>
          {attachments.map((item) => (
            <div key={item.id}>
              <span>
                {item.file.name} · {t(`chatWorkbench.upload.${item.status}`)}
              </span>
              {item.status === "failed" && (
                <Button
                  appearance="subtle"
                  onClick={() =>
                    void upload(item).catch(() =>
                      setAttachmentNotice("chatWorkbench.retryUploadFailed"),
                    )
                  }
                >
                  {t("chatWorkbench.retryFile", { name: item.file.name })}
                </Button>
              )}
              <Button
                appearance="subtle"
                icon={<DismissRegular />}
                aria-label={t("chatWorkbench.removeFile", {
                  name: item.file.name,
                })}
                onClick={() =>
                  setAttachments((current) =>
                    current.filter((candidate) => candidate.id !== item.id),
                  )
                }
              />
            </div>
          ))}
          <Button
            appearance="subtle"
            disabled={Boolean(activeTurnId) || submitting}
            onClick={() =>
              void adapter
                .sendMessage({ text: "", file: selectedFile })
                .catch(() =>
                  setAttachmentNotice(
                    (previous) => previous ?? "chatWorkbench.submissionFailed",
                  ),
                )
            }
          >
            {t("chatWorkbench.sendAttachments")}
          </Button>
        </div>
      )}
      {displayedRuntimeNotice && (
        <MessageBar intent="warning" className="agent-runtime-notice">
          <MessageBarBody>
            {displayedRuntimeNotice}
            {persistedNotice && (
              <Button
                appearance="subtle"
                size="small"
                icon={<ArrowSyncRegular />}
                onClick={() => void onRefresh()}
              >
                {t("chatWorkbench.reload")}
              </Button>
            )}
          </MessageBarBody>
        </MessageBar>
      )}
      <div className="agent-chat-surface">
        <ThemeProvider theme={agentTheme}>
          <AgentChatView
            adapter={adapter}
            empty={empty}
            selectedFile={selectedFile}
            onFileSelect={(file) => addFiles([file])}
            onClearFile={() => setAttachments((current) => current.slice(1))}
            onClearAttachments={() => setAttachments([])}
            renderAttachmentPicker={({ disabled }) => (
              <>
                <input
                  ref={picker}
                  type="file"
                  multiple
                  aria-label={t("chatWorkbench.selectFiles")}
                  data-testid="agent-multi-file-input"
                  style={{ display: "none" }}
                  disabled={disabled}
                  onChange={onPickerChange}
                />
                <Button
                  size="small"
                  appearance="subtle"
                  disabled={disabled}
                  onClick={() => picker.current?.click()}
                >
                  {t("chatWorkbench.addFile")}
                </Button>
              </>
            )}
            placeholder={t("chatWorkbench.placeholder")}
            composerLabels={{
              input: t("chatWorkbench.input"),
              send: t("chatWorkbench.send"),
              cancel: t("chatWorkbench.cancel"),
              addFile: t("chatWorkbench.addFile"),
              removeFile: (filename) =>
                t("chatWorkbench.removeFile", { name: filename }),
            }}
            emptyMessage={t("chatWorkbench.noMessages")}
            loadingMessage={t("chatWorkbench.reading")}
            genericErrorMessage={
              latestRun?.status === "failed"
                ? t("chatWorkbench.runFailed")
                : t("chatWorkbench.readFailed")
            }
            operationErrorMessage={t("chatWorkbench.noReply")}
            showTurnActions={false}
            showTimeline={false}
          />
        </ThemeProvider>
      </div>
    </section>
  );
}

export function AgentWorkbenchPage() {
  const { t } = useTranslation();
  const { tenantId, projectId, conversationId } = useParams();
  const navigate = useNavigate();
  const conversations = useAgentConversationsQuery(tenantId, projectId);
  const conversation = useAgentConversationQuery(
    tenantId,
    projectId,
    conversationId,
  );
  const refetchConversations = conversations.refetch;
  const refetchConversation = conversation.refetch;

  const base =
    tenantId && projectId
      ? `/app/${encodeURIComponent(tenantId)}/${encodeURIComponent(projectId)}/chat`
      : "/workspaces";

  const refresh = useCallback(async () => {
    await Promise.all([
      refetchConversations(),
      ...(conversationId ? [refetchConversation()] : []),
    ]);
  }, [conversationId, refetchConversation, refetchConversations]);

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

  function handleCreate() {
    navigate(base);
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
          creating={false}
          onCreate={handleCreate}
          onSelect={selectConversation}
          onRetry={() => void conversations.refetch()}
        />
        <div className="agent-workbench-content">
          {conversationId && conversation.isPending && (
            <div className="agent-workbench-loading">
              <Spinner label={t("chatWorkbench.loading")} />
            </div>
          )}
          {conversationId && conversation.isError && (
            <ErrorState
              title={t("chatWorkbench.conversationUnavailable")}
              detail={t("chatWorkbench.conversationUnavailableDetail")}
              onRetry={() => void conversation.refetch()}
            />
          )}
          {!conversationId && tenantId && projectId && (
            <AgentChat
              key={`${tenantId}:${projectId}:unsent`}
              conversation={emptyConversation}
              tenantId={tenantId}
              projectId={projectId}
              onRefresh={refresh}
              onFirstMessage={(id) => {
                void refetchConversations();
                navigate(`${base}/${encodeURIComponent(id)}`, {
                  replace: true,
                });
              }}
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
