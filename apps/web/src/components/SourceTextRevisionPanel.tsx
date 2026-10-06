import { useEffect, useState } from "react";
import {
  Button,
  Card,
  MessageBar,
  MessageBarBody,
  Spinner,
} from "@fluentui/react-components";
import { type JSONContent } from "@tiptap/core";
import { EditorContent, useEditor } from "@tiptap/react";
import StarterKit from "@tiptap/starter-kit";
import {
  type SourceSummary,
  type SourceVersionContent,
  useSaveSourceTextMutation,
  useSourceVersionContentQuery,
} from "../api/knowledge";
import { ApiError, createIdempotencyKey } from "../api/client";
import "./StructuredContentEditor.css";

// Source Markdown is edited as literal UTF-8 text. Formatting it as rich text
// and back would discard syntax not represented by the document block schema.
const extensions = [
  StarterKit.configure({
    blockquote: false,
    bold: false,
    code: false,
    codeBlock: false,
    dropcursor: false,
    gapcursor: false,
    hardBreak: false,
    heading: false,
    horizontalRule: false,
    italic: false,
    link: false,
    bulletList: false,
    orderedList: false,
    listItem: false,
    strike: false,
    trailingNode: false,
    underline: false,
  }),
];

export function textToEditorContent(value: string): JSONContent {
  return {
    type: "doc",
    content: value.split("\n").map((line) => ({
      type: "paragraph",
      content: line ? [{ type: "text", text: line }] : undefined,
    })),
  };
}

export function editorContentToText(content: JSONContent): string {
  if (content.type !== "doc" || !content.content?.length)
    throw new Error("当前正文结构无法保存为原始文本。");
  return content.content
    .map((paragraph) => {
      if (paragraph.type !== "paragraph" || paragraph.attrs?.geoBlockId)
        throw new Error("当前正文结构无法无损保存为原始文本。");
      return (paragraph.content ?? [])
        .map((node) => {
          if (node.type !== "text" || node.marks?.length)
            throw new Error("当前正文含有无法无损保存的格式。");
          return node.text ?? "";
        })
        .join("");
    })
    .join("\n");
}

interface Draft {
  baseVersionId: string;
  revision: number;
  mediaType: "text/plain" | "text/markdown";
  originalText: string;
  text: string;
  idempotencyKey: string;
}

function RawTextEditor({
  value,
  onChange,
  onUnavailable,
  disabled,
}: {
  value: string;
  onChange: (value: string) => void;
  onUnavailable: (reason: string | null) => void;
  disabled: boolean;
}) {
  const editor = useEditor({
    immediatelyRender: false,
    extensions,
    content: textToEditorContent(value),
    editable: !disabled,
    editorProps: {
      attributes: {
        "aria-label": "资料正文（原始文本）",
        role: "textbox",
        class: "structured-content-input",
      },
      handlePaste: (_view, event) => {
        const plainText = event.clipboardData?.getData("text/plain");
        if (plainText === undefined) return false;
        event.preventDefault();
        if (plainText)
          editor?.commands.insertContent({ type: "text", text: plainText });
        return true;
      },
    },
    onUpdate: ({ editor: current }) => {
      try {
        onUnavailable(null);
        onChange(editorContentToText(current.getJSON()));
      } catch (error) {
        onUnavailable((error as Error).message);
      }
    },
  });

  useEffect(() => {
    if (!editor) return;
    try {
      const roundTrip = editorContentToText(editor.getJSON());
      if (roundTrip !== value) {
        onUnavailable("此版本的空白或换行无法准确保留，暂不能编辑。");
      }
    } catch (error) {
      onUnavailable((error as Error).message);
    }
  }, [editor, value, onUnavailable]);
  useEffect(() => {
    if (editor && editor.isEditable === disabled)
      editor.setEditable(!disabled, false);
  }, [editor, disabled]);

  return (
    <div className="structured-content-editor">
      {!disabled && (
        <div
          className="structured-content-toolbar"
          role="toolbar"
          aria-label="资料编辑"
        >
          <Button
            size="small"
            onClick={() => editor?.chain().focus().undo().run()}
          >
            撤销
          </Button>
          <Button
            size="small"
            onClick={() => editor?.chain().focus().redo().run()}
          >
            重做
          </Button>
        </div>
      )}
      <EditorContent editor={editor} />
    </div>
  );
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
  const contentQuery = useSourceVersionContentQuery(
    tenantId,
    projectId,
    source.source_id,
    selectedVersionId,
  );
  const save = useSaveSourceTextMutation(tenantId, projectId);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [unavailable, setUnavailable] = useState<string | null>(null);
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
      setUnavailable(null);
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
              ? "编辑派生版本"
              : "原件版本"}
            {" · "}
            {selected.text_basis === "extracted"
              ? "从证据提取的草稿"
              : "精确保存的正文"}
            {" · "}
            {selected.media_type}
            {historical ? " · 历史只读" : ""}
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
                <p>以原始文本编辑，Markdown 标记会保留。</p>
                {unavailable ? (
                  <>
                    <MessageBar intent="warning">
                      <MessageBarBody>{unavailable}</MessageBarBody>
                    </MessageBar>
                    <pre className="source-original-text">{draft.text}</pre>
                  </>
                ) : (
                  <RawTextEditor
                    key={editorKey}
                    value={draft.text}
                    disabled={save.isPending || conflicted || mismatch}
                    onUnavailable={setUnavailable}
                    onChange={(text) => {
                      setDraft(
                        (previous) =>
                          previous && {
                            ...previous,
                            text,
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
                    !!unavailable ||
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
