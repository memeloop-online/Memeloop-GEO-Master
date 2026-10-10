import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Button,
  Card,
  MessageBar,
  MessageBarBody,
  Spinner,
} from "@fluentui/react-components";
import {
  type SourceSummary,
  type SourceVersionContent,
  useSaveSourceTextMutation,
  useSourceVersionContentQuery,
} from "../api/knowledge";
import { ApiError, createIdempotencyKey } from "../api/client";
import { MarkdownSourceEditor } from "./MarkdownSourceEditor";
import "./StructuredContentEditor.css";

interface Draft {
  baseVersionId: string;
  revision: number;
  mediaType: "text/plain" | "text/markdown";
  originalText: string;
  text: string;
  idempotencyKey: string;
}

export function preserveSourceLineEndings(
  text: string,
  original: string,
): string {
  return original.includes("\r\n") &&
    !original.replaceAll("\r\n", "").includes("\n")
    ? text.replaceAll("\r\n", "\n").replaceAll("\n", "\r\n")
    : text;
}

export function SourceTextRevisionPanel({
  tenantId,
  projectId,
  source,
  selectedVersionId,
  canEdit,
  onViewLatest,
}: {
  tenantId: string;
  projectId: string;
  source: SourceSummary;
  selectedVersionId: string;
  canEdit: boolean;
  onViewLatest: () => void;
}) {
  const { t } = useTranslation();
  const contentQuery = useSourceVersionContentQuery(
    tenantId,
    projectId,
    source.source_id,
    selectedVersionId,
  );
  const save = useSaveSourceTextMutation(tenantId, projectId);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [conflicted, setConflicted] = useState(false);
  const content = contentQuery.data;
  const historical = selectedVersionId !== source.current_version_id;
  const mediaType = content?.media_type;
  const supported = mediaType === "text/plain" || mediaType === "text/markdown";
  const editing =
    canEdit && source.state === "active" && supported && !historical;

  useEffect(() => {
    if (
      !content ||
      !supported ||
      historical ||
      !canEdit ||
      content.source_version_id !== source.current_version_id
    )
      return;
    if (
      !draft ||
      (draft.text === draft.originalText &&
        draft.baseVersionId !== content.source_version_id)
    ) {
      setDraft({
        baseVersionId: content.source_version_id,
        revision: source.revision,
        mediaType: mediaType as Draft["mediaType"],
        originalText: content.text,
        text: content.text,
        idempotencyKey: createIdempotencyKey(),
      });
      setConflicted(false);
    }
  }, [
    content,
    supported,
    historical,
    canEdit,
    draft,
    source.revision,
    mediaType,
  ]);

  const selected: SourceVersionContent | undefined = content;
  const mismatch = Boolean(
    draft && draft.baseVersionId !== source.current_version_id,
  );
  const dirty = Boolean(draft && draft.text !== draft.originalText);
  const textTooLarge =
    draft && new TextEncoder().encode(draft.text).length > 256 * 1024;
  const editorKey = draft ? `${draft.baseVersionId}:${draft.revision}` : "";

  return (
    <Card className="source-text-panel" style={{ minWidth: 0 }}>
      <h2>资料正文</h2>
      {contentQuery.isPending ? (
        <Spinner size="tiny" label="正在读取版本正文" />
      ) : null}
      {contentQuery.isError && (
        <MessageBar intent="warning">
          <MessageBarBody>
            无法读取完整正文，请重试。
            <Button size="small" onClick={() => void contentQuery.refetch()}>
              重试读取
            </Button>
          </MessageBarBody>
        </MessageBar>
      )}
      {selected && (
        <>
          <p>
            {selected.representation === "authored_text"
              ? t("knowledgeEditor.authored")
              : t("knowledgeEditor.original")}
            {" · "}
            {selected.media_type === "text/markdown"
              ? t("knowledgeEditor.markdownFormat")
              : selected.media_type === "text/plain"
                ? t("knowledgeEditor.plainFormat")
                : selected.media_type}
            {selected.text_basis === "extracted"
              ? ` · ${t("knowledgeEditor.extracted")}`
              : ""}
            {historical ? ` · ${t("knowledgeEditor.historical")}` : ""}
          </p>
          {historical || !editing ? (
            <pre className="source-original-text">{selected.text}</pre>
          ) : (
            draft && (
              <>
                {selected.text_basis === "extracted" && (
                  <MessageBar intent="warning">
                    <MessageBarBody>
                      这是提取草稿。保存后将创建新版本，原件和旧证据保留。
                    </MessageBarBody>
                  </MessageBar>
                )}
                <p>
                  {draft.mediaType === "text/markdown"
                    ? t("knowledgeEditor.markdownHint")
                    : t("knowledgeEditor.plainHint")}
                </p>
                {draft.mediaType === "text/markdown" ? (
                  <MarkdownSourceEditor
                    key={editorKey}
                    value={draft.text}
                    disabled={save.isPending || conflicted || mismatch}
                    onChange={(text) => {
                      setDraft(
                        (previous) =>
                          previous && {
                            ...previous,
                            text: preserveSourceLineEndings(
                              text,
                              previous.originalText,
                            ),
                            idempotencyKey: createIdempotencyKey(),
                          },
                      );
                    }}
                  />
                ) : (
                  <textarea
                    aria-label={t("knowledgeEditor.rawInput")}
                    className="source-textarea"
                    spellCheck={false}
                    value={draft.text}
                    disabled={save.isPending || conflicted || mismatch}
                    onChange={(event) => {
                      setDraft(
                        (previous) =>
                          previous && {
                            ...previous,
                            text: preserveSourceLineEndings(
                              event.target.value,
                              previous.originalText,
                            ),
                            idempotencyKey: createIdempotencyKey(),
                          },
                      );
                    }}
                  />
                )}
                {textTooLarge && (
                  <p role="alert">
                    正文超过 256 KiB，不能保存；内容不会被截断。
                  </p>
                )}
                {(conflicted || mismatch) && (
                  <MessageBar intent="warning">
                    <MessageBarBody>
                      来源已更新，草稿已保留。请查看最新版本再继续。
                      <Button size="small" onClick={onViewLatest}>
                        查看最新版本
                      </Button>
                      {draft.baseVersionId !== source.current_version_id &&
                        selected.source_version_id ===
                          source.current_version_id && (
                          <Button
                            size="small"
                            onClick={() => {
                              if (!draft) return;
                              setDraft({
                                ...draft,
                                baseVersionId: source.current_version_id!,
                                revision: source.revision,
                                originalText: selected.text,
                                idempotencyKey: createIdempotencyKey(),
                              });
                              setConflicted(false);
                            }}
                          >
                            以最新版本为基线修订
                          </Button>
                        )}
                    </MessageBarBody>
                  </MessageBar>
                )}
                {save.isError && !conflicted && (
                  <MessageBar intent="error">
                    <MessageBarBody>
                      保存失败：{save.error.message}。草稿已保留，可重试。
                    </MessageBarBody>
                  </MessageBar>
                )}
                <Button
                  appearance="primary"
                  disabled={
                    !dirty ||
                    !!textTooLarge ||
                    save.isPending ||
                    conflicted ||
                    mismatch
                  }
                  onClick={() => {
                    if (!draft) return;
                    void save
                      .mutateAsync({
                        sourceId: source.source_id,
                        revision: draft.revision,
                        baseVersionId: draft.baseVersionId,
                        mediaType: draft.mediaType,
                        text: draft.text,
                        idempotencyKey: draft.idempotencyKey,
                      })
                      .then((receipt) => {
                        setDraft({
                          baseVersionId:
                            receipt.source_version.source_version_id,
                          revision: receipt.source.revision,
                          mediaType: draft.mediaType,
                          originalText: draft.text,
                          text: draft.text,
                          idempotencyKey: createIdempotencyKey(),
                        });
                        onViewLatest();
                      })
                      .catch((error: unknown) => {
                        if (
                          error instanceof ApiError &&
                          error.code === "source_revision_conflict"
                        )
                          setConflicted(true);
                      });
                  }}
                >
                  {save.isPending ? "正在保存…" : "保存为新版本"}
                </Button>
                {save.isSuccess && !dirty && (
                  <p role="status">新版本已保存。</p>
                )}
              </>
            )
          )}
          {!historical && !supported && <p>此格式暂不能编辑。</p>}
          {!historical && !canEdit && <p>当前成员只有读取权限，正文只读。</p>}
        </>
      )}
    </Card>
  );
}
