import { useEffect, useRef, useState } from "react";
import {
  Button,
  Input,
  Popover,
  PopoverSurface,
  PopoverTrigger,
  Toolbar,
  ToolbarButton,
  ToolbarToggleButton,
} from "@fluentui/react-components";
import {
  ArrowRedoRegular,
  ArrowUndoRegular,
  CodeBlockRegular,
  CodeRegular,
  LinkRegular,
  TableRegular,
  TextBoldRegular,
  TextBulletListLtrRegular,
  TextHeader2Regular,
  TextItalicRegular,
  TextNumberListLtrRegular,
} from "@fluentui/react-icons";
import type { MarkdownManager } from "@tiptap/markdown";
import { Markdown } from "@tiptap/markdown";
import { EditorContent, useEditor, useEditorState } from "@tiptap/react";
import StarterKit from "@tiptap/starter-kit";
import { TableKit } from "@tiptap/extension-table";
import { useTranslation } from "react-i18next";
import "./StructuredContentEditor.css";

const extensions = [
  StarterKit.configure({
    blockquote: false,
    horizontalRule: false,
    strike: false,
    underline: false,
    link: { openOnClick: false },
  }),
  TableKit,
  Markdown,
];

// The library's lexer identifies constructs the editor does not represent. Keep
// those documents in a byte-preserving source field, including raw HTML.
const supportedTokens = new Set([
  "space",
  "heading",
  "paragraph",
  "text",
  "strong",
  "em",
  "codespan",
  "code",
  "br",
  "link",
  "list",
  "list_item",
  "table",
  "escape",
]);

type Token = {
  type: string;
  tokens?: Token[];
  items?: Token[];
  header?: { tokens?: Token[] }[];
  rows?: { tokens?: Token[] }[][];
  task?: boolean;
};

function supportedTree(tokens: Token[]): boolean {
  return tokens.every(
    (token) =>
      supportedTokens.has(token.type) &&
      !token.task &&
      (!token.tokens || supportedTree(token.tokens)) &&
      (!token.items || supportedTree(token.items)) &&
      (!token.header ||
        token.header.every((cell) => supportedTree(cell.tokens ?? []))) &&
      (!token.rows ||
        token.rows.every((row) =>
          row.every((cell) => supportedTree(cell.tokens ?? [])),
        )),
  );
}

export function canEditMarkdownVisually(
  text: string,
  manager: MarkdownManager | undefined,
): boolean {
  if (!manager) return false;
  try {
    // CRLF, trailing whitespace and source spelling are still preserved until
    // a user actually edits. A parsed document is never written automatically.
    const tokens = manager.instance.lexer(text) as Token[];
    if (!supportedTree(tokens)) return false;
    const parsed = manager.parse(text);
    return (
      JSON.stringify(manager.parse(manager.serialize(parsed))) ===
      JSON.stringify(parsed)
    );
  } catch {
    return false;
  }
}

export function MarkdownSourceEditor({
  value,
  onChange,
  disabled,
}: {
  value: string;
  onChange: (value: string) => void;
  disabled: boolean;
}) {
  const { t } = useTranslation();
  const [sourceMode, setSourceMode] = useState(false);
  const [pasteNotice, setPasteNotice] = useState(false);
  const [linkHref, setLinkHref] = useState("");
  const [linkError, setLinkError] = useState(false);
  const [linkSelectedOnOpen, setLinkSelectedOnOpen] = useState(false);
  const editor = useEditor({
    immediatelyRender: false,
    extensions,
    content: value,
    contentType: "markdown",
    editable: !disabled,
    editorProps: {
      attributes: {
        "aria-label": t("knowledgeEditor.visualInput"),
        role: "textbox",
        class: "structured-content-input",
      },
      handlePaste: (_view, event) => {
        if (!event.clipboardData) return false;
        event.preventDefault();
        const plain = event.clipboardData.getData("text/plain");
        if (!canEditMarkdownVisually(plain, editor?.markdown)) {
          setSourceMode(true);
          setPasteNotice(true);
          return true;
        }
        if (plain)
          editor?.commands.insertContent(plain, { contentType: "markdown" });
        return true;
      },
    },
    onUpdate: ({ editor: current }) => {
      onChange(current.getMarkdown());
    },
  });
  const visualAvailable = canEditMarkdownVisually(value, editor?.markdown);
  const raw = sourceMode || !visualAvailable;
  const previousRaw = useRef(raw);
  const active = useEditorState({
    editor,
    selector: ({ editor: current }) => ({
      heading: current?.isActive("heading", { level: 2 }) ?? false,
      bold: current?.isActive("bold") ?? false,
      italic: current?.isActive("italic") ?? false,
      link: current?.isActive("link") ?? false,
      bullets: current?.isActive("bulletList") ?? false,
      numbered: current?.isActive("orderedList") ?? false,
      inlineCode: current?.isActive("code") ?? false,
      codeBlock: current?.isActive("codeBlock") ?? false,
    }),
  });
  const checkedValues = {
    format: Object.entries(active ?? {})
      .filter(([, selected]) => selected)
      .map(([name]) => name),
  };

  useEffect(() => {
    if (editor && editor.isEditable === disabled)
      editor.setEditable(!disabled, false);
  }, [editor, disabled]);

  // When switching from source, replace the editor document without emitting
  // an update: the draft already contains exactly what the user typed.
  useEffect(() => {
    if (editor && previousRaw.current && !raw && editor.getMarkdown() !== value)
      editor.commands.setContent(value, {
        contentType: "markdown",
        emitUpdate: false,
      });
    previousRaw.current = raw;
  }, [editor, raw, value]);

  return (
    <div className="structured-content-editor">
      <Toolbar
        className="structured-content-toolbar"
        aria-label={t("knowledgeEditor.toolbar")}
        size="small"
        checkedValues={checkedValues}
      >
        {visualAvailable && (
          <ToolbarButton
            disabled={disabled}
            onClick={() => {
              setPasteNotice(false);
              setSourceMode(!sourceMode);
            }}
          >
            {raw
              ? t("knowledgeEditor.visualMode")
              : t("knowledgeEditor.sourceMode")}
          </ToolbarButton>
        )}
        {!raw && !disabled && (
          <>
            <ToolbarToggleButton
              name="format"
              value="heading"
              appearance="subtle"
              icon={<TextHeader2Regular />}
              aria-label={t("knowledgeEditor.heading")}
              title={t("knowledgeEditor.heading")}
              onClick={() =>
                editor?.chain().focus().toggleHeading({ level: 2 }).run()
              }
            />
            <ToolbarToggleButton
              name="format"
              value="bold"
              appearance="subtle"
              icon={<TextBoldRegular />}
              aria-label={t("knowledgeEditor.bold")}
              title={t("knowledgeEditor.bold")}
              onClick={() => editor?.chain().focus().toggleBold().run()}
            />
            <ToolbarToggleButton
              name="format"
              value="italic"
              appearance="subtle"
              icon={<TextItalicRegular />}
              aria-label={t("knowledgeEditor.italic")}
              title={t("knowledgeEditor.italic")}
              onClick={() => editor?.chain().focus().toggleItalic().run()}
            />
            <Popover
              onOpenChange={(_, data) => {
                if (data.open) {
                  const selectedLink = editor?.getAttributes("link").href;
                  setLinkHref(String(selectedLink ?? ""));
                  setLinkSelectedOnOpen(Boolean(selectedLink));
                  setLinkError(false);
                }
              }}
            >
              <PopoverTrigger disableButtonEnhancement>
                <ToolbarButton
                  appearance="subtle"
                  icon={<LinkRegular />}
                  aria-label={t("knowledgeEditor.link")}
                  title={t("knowledgeEditor.link")}
                  aria-pressed={active?.link ?? false}
                />
              </PopoverTrigger>
              <PopoverSurface className="source-link-popover">
                <Input
                  size="small"
                  type="url"
                  aria-label={t("knowledgeEditor.linkUrl")}
                  placeholder="https://"
                  value={linkHref}
                  onChange={(_, data) => setLinkHref(data.value)}
                />
                <Button
                  size="small"
                  disabled={!linkHref.trim()}
                  onClick={() => {
                    const accepted = editor
                      ?.chain()
                      .focus()
                      .extendMarkRange("link")
                      .setLink({
                        href: linkHref.trim(),
                      })
                      .run();
                    setLinkError(!accepted);
                  }}
                >
                  {t("knowledgeEditor.link")}
                </Button>
                {linkSelectedOnOpen && (
                  <Button
                    size="small"
                    onClick={() => {
                      editor
                        ?.chain()
                        .focus()
                        .extendMarkRange("link")
                        .unsetLink()
                        .run();
                      setLinkError(false);
                      setLinkSelectedOnOpen(false);
                    }}
                  >
                    {t("knowledgeEditor.unlink")}
                  </Button>
                )}
                {linkError && (
                  <span role="alert">{t("knowledgeEditor.invalidLink")}</span>
                )}
              </PopoverSurface>
            </Popover>
            <ToolbarToggleButton
              name="format"
              value="bullets"
              appearance="subtle"
              icon={<TextBulletListLtrRegular />}
              aria-label={t("knowledgeEditor.bullets")}
              title={t("knowledgeEditor.bullets")}
              onClick={() => editor?.chain().focus().toggleBulletList().run()}
            />
            <ToolbarToggleButton
              name="format"
              value="numbered"
              appearance="subtle"
              icon={<TextNumberListLtrRegular />}
              aria-label={t("knowledgeEditor.numbered")}
              title={t("knowledgeEditor.numbered")}
              onClick={() => editor?.chain().focus().toggleOrderedList().run()}
            />
            <ToolbarToggleButton
              name="format"
              value="inlineCode"
              appearance="subtle"
              icon={<CodeRegular />}
              aria-label={t("knowledgeEditor.inlineCode")}
              title={t("knowledgeEditor.inlineCode")}
              onClick={() => editor?.chain().focus().toggleCode().run()}
            />
            <ToolbarToggleButton
              name="format"
              value="codeBlock"
              appearance="subtle"
              icon={<CodeBlockRegular />}
              aria-label={t("knowledgeEditor.codeBlock")}
              title={t("knowledgeEditor.codeBlock")}
              onClick={() => editor?.chain().focus().toggleCodeBlock().run()}
            />
            <ToolbarButton
              appearance="subtle"
              icon={<TableRegular />}
              aria-label={t("knowledgeEditor.table")}
              title={t("knowledgeEditor.table")}
              onClick={() =>
                editor
                  ?.chain()
                  .focus()
                  .insertTable({ rows: 2, cols: 2, withHeaderRow: true })
                  .run()
              }
            />
            <ToolbarButton
              appearance="subtle"
              icon={<ArrowUndoRegular />}
              aria-label={t("knowledgeEditor.undo")}
              title={t("knowledgeEditor.undo")}
              onClick={() => editor?.chain().focus().undo().run()}
            />
            <ToolbarButton
              appearance="subtle"
              icon={<ArrowRedoRegular />}
              aria-label={t("knowledgeEditor.redo")}
              title={t("knowledgeEditor.redo")}
              onClick={() => editor?.chain().focus().redo().run()}
            />
          </>
        )}
      </Toolbar>
      {raw ? (
        <>
          {pasteNotice && (
            <p role="status" className="source-editor-note">
              {t("knowledgeEditor.pasteInSource")}
            </p>
          )}
          {!visualAvailable && (
            <p className="source-editor-note">
              {t("knowledgeEditor.rawFallback")}
            </p>
          )}
          <textarea
            aria-label={t("knowledgeEditor.rawInput")}
            className="source-textarea"
            spellCheck={false}
            value={value}
            disabled={disabled}
            onChange={(event) => onChange(event.target.value)}
          />
        </>
      ) : (
        <EditorContent editor={editor} data-testid="knowledge-visual-editor" />
      )}
    </div>
  );
}
