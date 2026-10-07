import { useEffect, useRef, useState } from "react";
import {
  Button,
  Input,
  Menu,
  MenuItem,
  MenuList,
  MenuPopover,
  MenuTrigger,
  Popover,
  PopoverSurface,
  PopoverTrigger,
  Toolbar,
  ToolbarButton,
  ToolbarToggleButton,
} from "@fluentui/react-components";
import { TableKit } from "@tiptap/extension-table";
import { Extension, type JSONContent } from "@tiptap/core";
import { Plugin } from "@tiptap/pm/state";
import { EditorContent, useEditor, useEditorState } from "@tiptap/react";
import StarterKit from "@tiptap/starter-kit";
import { useTranslation } from "react-i18next";
import type { ContentBlock, StructuredDocument } from "../api/content";
import { fromRichNode, isSimpleLegacy, toRichNode } from "./richContentAdapter";
import "./StructuredContentEditor.css";

const metadata = Extension.create({
  name: "geoContentMetadata",
  addProseMirrorPlugins() {
    return [
      new Plugin({
        appendTransaction: (_steps, _before, state) => {
          const seen = new Set<string>();
          const transaction = state.tr;
          state.doc.forEach((node, position) => {
            if (node.type.name === "paragraph" && node.attrs.geoCaptionFor)
              return;
            if (
              ![
                "heading",
                "paragraph",
                "bulletList",
                "orderedList",
                "codeBlock",
                "table",
              ].includes(node.type.name)
            )
              return;
            const inherited = ["bulletList", "orderedList"].includes(
              node.type.name,
            )
              ? node.content.firstChild?.content.firstChild?.attrs
              : undefined;
            const id = node.attrs.geoBlockId ?? inherited?.geoBlockId;
            if (typeof id === "string" && !seen.has(id)) {
              seen.add(id);
              if (!node.attrs.geoBlockId)
                transaction.setNodeMarkup(position, undefined, {
                  ...node.attrs,
                  geoBlockId: id,
                  geoCitations: inherited?.geoCitations ?? [],
                });
              return;
            }
            const next = crypto.randomUUID();
            seen.add(next);
            transaction.setNodeMarkup(position, undefined, {
              ...node.attrs,
              geoBlockId: next,
            });
          });
          return transaction.docChanged ? transaction : null;
        },
      }),
    ];
  },
  addGlobalAttributes() {
    return [
      {
        types: [
          "heading",
          "paragraph",
          "bulletList",
          "orderedList",
          "codeBlock",
          "table",
        ],
        attributes: {
          geoBlockId: {
            default: null,
            parseHTML: () => null,
            renderHTML: (attributes) =>
              attributes.geoBlockId
                ? { "data-geo-block-id": attributes.geoBlockId }
                : {},
          },
          geoCitations: {
            default: [],
            parseHTML: () => [],
            renderHTML: (attributes) => ({
              "data-geo-citations": JSON.stringify(
                attributes.geoCitations ?? [],
              ),
            }),
          },
          geoCaptionFor: {
            default: null,
            parseHTML: () => null,
            renderHTML: (attributes) =>
              attributes.geoCaptionFor
                ? { "data-geo-caption-for": attributes.geoCaptionFor }
                : {},
          },
        },
      },
    ];
  },
});

const extensions = [
  StarterKit.configure({
    blockquote: false,
    dropcursor: false,
    gapcursor: false,
    horizontalRule: false,
    trailingNode: false,
    heading: { levels: [1, 2, 3, 4, 5, 6] },
    link: { openOnClick: false },
  }),
  TableKit,
  Extension.create({
    name: "geoLinkTitle",
    addGlobalAttributes() {
      return [
        {
          types: ["link"],
          attributes: {
            title: {
              default: null,
              parseHTML: (element) => element.getAttribute("title"),
              renderHTML: (attributes) =>
                attributes.title ? { title: attributes.title } : {},
            },
          },
        },
      ];
    },
  }),
  metadata,
];

function text(value: string): JSONContent[] {
  return value ? [{ type: "text", text: value }] : [];
}

function attributes(block: ContentBlock) {
  return {
    geoBlockId: block.block_id,
    geoCitations: [...block.citation_ids],
  };
}

export function documentToEditor(document: StructuredDocument): JSONContent {
  return {
    type: "doc",
    content: document.blocks.flatMap((block): JSONContent[] => {
      if (block.kind === "rich") {
        if (block.rich?.version !== 1)
          throw new Error("This content version is not supported.");
        const node = fromRichNode(block.rich.node);
        return [{ ...node, attrs: { ...node.attrs, ...attributes(block) } }];
      }
      if (!["heading", "paragraph", "list"].includes(block.kind))
        throw new Error("This content structure is not supported.");
      if (block.kind !== "list")
        return [
          {
            type: block.kind === "heading" ? "heading" : "paragraph",
            attrs: {
              ...attributes(block),
              ...(block.kind === "heading" ? { level: 2 } : {}),
            },
            content: text(block.text),
          },
        ];
      const list: JSONContent = {
        type: "bulletList",
        attrs: attributes(block),
        content: block.items.map((item) => ({
          type: "listItem",
          content: [{ type: "paragraph", content: text(item) }],
        })),
      };
      if (!block.text) return [list];
      return [
        {
          type: "paragraph",
          attrs: { ...attributes(block), geoCaptionFor: block.block_id },
          content: text(block.text),
        },
        list,
      ];
    }),
  };
}

function nodeText(node: JSONContent): string {
  if (node.type === "text") return node.text ?? "";
  if (node.type === "hardBreak") return "\n";
  return (node.content ?? []).map(nodeText).join("");
}

function nodeParagraphs(node: JSONContent): string {
  return (node.content ?? []).map(nodeText).join("\n");
}

/**
 * This is an adapter, not another editing engine. Reject schema shapes that
 * cannot be represented by ContentBlock instead of silently flattening them.
 */
export function editorToDocument(
  content: JSONContent,
  previous: StructuredDocument,
): StructuredDocument {
  const seen = new Set<string>();
  const blocks: ContentBlock[] = [];
  const nodes = content.content ?? [];
  const previousById = new Map(
    previous.blocks.map((block) => [block.block_id, block]),
  );
  const metadataFor = (node: JSONContent) => {
    const candidate = node.attrs?.geoBlockId;
    const blockId =
      typeof candidate === "string" && !seen.has(candidate)
        ? candidate
        : crypto.randomUUID();
    seen.add(blockId);
    const ids = node.attrs?.geoCitations;
    const allowed = new Set(
      previous.blocks.flatMap((block) => block.citation_ids),
    );
    if (
      !Array.isArray(ids) ||
      ids.some((id) => typeof id !== "string" || !allowed.has(id))
    )
      throw new Error("正文包含无法核对的引用，请刷新版本后重试。");
    return { block_id: blockId, citation_ids: ids as string[] };
  };
  for (let index = 0; index < nodes.length; index++) {
    const node = nodes[index];
    if (node.type === "bulletList" || node.type === "orderedList") {
      const originalIds = new Set(
        (node.content ?? [])
          .flatMap((item) => item.content ?? [])
          .map((paragraph) => paragraph.attrs?.geoBlockId)
          .filter(
            (id): id is string =>
              typeof id === "string" && previousById.has(id),
          ),
      );
      const originalList = previousById.get(node.attrs?.geoBlockId);
      const [firstId] = originalIds;
      node.attrs = {
        ...node.attrs,
        geoBlockId:
          originalList?.kind === "list" || originalList?.kind === "rich"
            ? node.attrs?.geoBlockId
            : (firstId ?? node.attrs?.geoBlockId),
        geoCitations: [
          ...new Set([
            ...(node.attrs?.geoCitations ?? []),
            ...[...originalIds].flatMap(
              (id) => previousById.get(id)?.citation_ids ?? [],
            ),
          ]),
        ],
      };
    }
    if (node.type === "paragraph" && node.attrs?.geoCaptionFor) {
      const next = nodes[index + 1];
      if (
        isSimpleLegacy(node) &&
        next?.type === "bulletList" &&
        isSimpleLegacy(next) &&
        previousById.get(next.attrs?.geoBlockId)?.kind === "list" &&
        node.attrs.geoCaptionFor === next.attrs?.geoBlockId
      ) {
        const attrs = metadataFor(next);
        blocks.push({
          ...attrs,
          kind: "list",
          text: nodeText(node),
          items: listItems(next),
        });
        index++;
        continue;
      }
    }
    const attrs = metadataFor(node);
    const {
      geoBlockId: _id,
      geoCitations: _citations,
      geoCaptionFor: _caption,
      ...nodeAttrs
    } = node.attrs ?? {};
    void _id;
    void _citations;
    void _caption;
    const normalized = toRichNode({ ...node, attrs: nodeAttrs });
    const prior = previousById.get(attrs.block_id);
    if (
      prior?.kind === "rich" &&
      prior.rich?.version === 1 &&
      JSON.stringify(toRichNode(prior.rich.node)) === JSON.stringify(normalized)
    )
      blocks.push({
        ...prior,
        citation_ids: attrs.citation_ids,
      });
    else if (
      (node.type === "heading" || node.type === "paragraph") &&
      isSimpleLegacy(node) &&
      !!nodeText(node).trim() &&
      (node.type !== "heading" || node.attrs?.level === 2) &&
      prior?.kind !== "rich"
    )
      blocks.push({
        ...attrs,
        kind: node.type,
        text: nodeText(node),
        items: [],
      });
    else if (
      node.type === "bulletList" &&
      isSimpleLegacy(node) &&
      prior?.kind !== "rich"
    )
      blocks.push({ ...attrs, kind: "list", text: "", items: listItems(node) });
    else
      blocks.push({
        ...attrs,
        kind: "rich",
        text: "",
        items: [],
        rich: { version: 1, node: normalized },
      });
  }
  if (
    !blocks.length ||
    blocks.some(
      (block) =>
        (block.kind === "list" && !block.items.length) ||
        (block.kind !== "rich" &&
          !block.text.trim() &&
          !block.items.some((item) => item.trim())),
    )
  )
    throw new Error("正文至少需要一个非空内容块；空白块尚不能保存。");
  return {
    title: previous.title,
    blocks,
    ...(previous.schema_version === 2 ||
    blocks.some((block) => block.kind === "rich")
      ? { schema_version: 2 as const }
      : {}),
  };
}

function listItems(list: JSONContent): string[] {
  const children = list.content ?? [];
  if (
    !children.length ||
    children.some(
      (item) =>
        item.type !== "listItem" ||
        !item.content?.length ||
        item.content.some((entry) => entry.type !== "paragraph"),
    )
  )
    throw new Error("当前版本不支持嵌套列表或非文本列表项。");
  return children.map(nodeParagraphs);
}

interface Props {
  document: StructuredDocument;
  baselineDocument?: StructuredDocument;
  readonly: boolean;
  onChange: (document: StructuredDocument | null, error: string | null) => void;
}

export function StructuredContentEditor({
  document,
  baselineDocument,
  readonly,
  onChange,
}: Props) {
  const { t } = useTranslation();
  const original = useRef(baselineDocument ?? document);
  original.current = baselineDocument ?? document;
  const [linkHref, setLinkHref] = useState("");
  const [linkError, setLinkError] = useState(false);
  const [linkSelected, setLinkSelected] = useState(false);
  const [pasteError, setPasteError] = useState(false);
  let loaded: JSONContent | undefined;
  let unsupported = false;
  try {
    loaded = documentToEditor(document);
  } catch {
    unsupported = true;
  }
  const editor = useEditor({
    immediatelyRender: false,
    extensions,
    editable: !readonly && !unsupported,
    content: loaded ?? { type: "doc", content: [{ type: "paragraph" }] },
    editorProps: {
      attributes: {
        "aria-label": t("generatedEditor.input"),
        role: "textbox",
        class: "structured-content-input",
      },
      handlePaste: (_view, event) => {
        if (event.clipboardData?.types.includes("text/html")) {
          event.preventDefault();
          setPasteError(true);
          return true;
        }
        return false;
      },
    },
    onUpdate: ({ editor: current }) => {
      try {
        onChange(editorToDocument(current.getJSON(), original.current), null);
      } catch {
        onChange(null, t("generatedEditor.formatError"));
      }
    },
  });

  useEffect(() => {
    if (editor && editor.isEditable === (readonly || unsupported))
      editor.setEditable(!readonly && !unsupported, false);
  }, [editor, readonly, unsupported]);
  const active = useEditorState({
    editor,
    selector: ({ editor: current }) => ({
      heading: current?.isActive("heading", { level: 2 }) ?? false,
      bold: current?.isActive("bold") ?? false,
      italic: current?.isActive("italic") ?? false,
      strike: current?.isActive("strike") ?? false,
      underline: current?.isActive("underline") ?? false,
      code: current?.isActive("code") ?? false,
      link: current?.isActive("link") ?? false,
      bullets: current?.isActive("bulletList") ?? false,
      numbered: current?.isActive("orderedList") ?? false,
      codeBlock: current?.isActive("codeBlock") ?? false,
    }),
  });

  const toParagraph = () => {
    if (!editor) return;
    const selection = editor.state.selection.$from;
    let depth = selection.depth;
    while (
      depth > 0 &&
      !["bulletList", "orderedList"].includes(selection.node(depth).type.name)
    )
      depth--;
    if (!depth) {
      editor.chain().focus().setParagraph().run();
      return;
    }
    // Before the library unwraps a list, carry its evidence to each native
    // list-item paragraph. Its first paragraph keeps the original block ID.
    const list = selection.node(depth);
    const transaction = editor.state.tr;
    let itemPosition = selection.before(depth) + 1;
    let first = true;
    list.forEach((item) => {
      let paragraphPosition = itemPosition + 1;
      item.forEach((paragraph) => {
        transaction.setNodeMarkup(paragraphPosition, undefined, {
          ...paragraph.attrs,
          geoBlockId: first ? list.attrs.geoBlockId : crypto.randomUUID(),
          geoCitations: [...(list.attrs.geoCitations ?? [])],
          geoCaptionFor: null,
        });
        first = false;
        paragraphPosition += paragraph.nodeSize;
      });
      itemPosition += item.nodeSize;
    });
    editor.view.dispatch(transaction);
    if (list.type.name === "orderedList")
      editor.chain().focus().toggleOrderedList().setParagraph().run();
    else editor.chain().focus().toggleBulletList().setParagraph().run();
  };

  return (
    <div className="structured-content-editor generated-content-editor">
      {unsupported && <p role="alert">{t("generatedEditor.unsupported")}</p>}
      {pasteError && (
        <p role="alert">{t("generatedEditor.pasteUnsupported")}</p>
      )}
      {!readonly && !unsupported && (
        <Toolbar
          className="structured-content-toolbar"
          aria-label={t("generatedEditor.toolbar")}
          size="small"
          checkedValues={{
            format: Object.entries(active ?? {})
              .filter(([, selected]) => selected)
              .map(([key]) => key),
          }}
        >
          <ToolbarButton onClick={toParagraph}>
            {t("generatedEditor.paragraph")}
          </ToolbarButton>
          <ToolbarToggleButton
            name="format"
            value="heading"
            onClick={() =>
              editor?.chain().focus().toggleHeading({ level: 2 }).run()
            }
          >
            {t("generatedEditor.heading")}
          </ToolbarToggleButton>
          <Menu>
            <MenuTrigger disableButtonEnhancement>
              <ToolbarButton>{t("generatedEditor.headingLevel")}</ToolbarButton>
            </MenuTrigger>
            <MenuPopover>
              <MenuList>
                {([1, 2, 3, 4, 5, 6] as const).map((level) => (
                  <MenuItem
                    key={level}
                    onClick={() =>
                      editor?.chain().focus().setHeading({ level }).run()
                    }
                  >
                    {t("generatedEditor.level", { level })}
                  </MenuItem>
                ))}
              </MenuList>
            </MenuPopover>
          </Menu>
          <ToolbarToggleButton
            name="format"
            value="bold"
            onClick={() => editor?.chain().focus().toggleBold().run()}
          >
            {t("generatedEditor.bold")}
          </ToolbarToggleButton>
          <ToolbarToggleButton
            name="format"
            value="italic"
            onClick={() => editor?.chain().focus().toggleItalic().run()}
          >
            {t("generatedEditor.italic")}
          </ToolbarToggleButton>
          <ToolbarToggleButton
            name="format"
            value="strike"
            onClick={() => editor?.chain().focus().toggleStrike().run()}
          >
            {t("generatedEditor.strike")}
          </ToolbarToggleButton>
          <ToolbarToggleButton
            name="format"
            value="underline"
            onClick={() => editor?.chain().focus().toggleUnderline().run()}
          >
            {t("generatedEditor.underline")}
          </ToolbarToggleButton>
          <ToolbarToggleButton
            name="format"
            value="code"
            onClick={() => editor?.chain().focus().toggleCode().run()}
          >
            {t("generatedEditor.code")}
          </ToolbarToggleButton>
          <Popover
            onOpenChange={(_, data) => {
              if (data.open) {
                const href = editor?.getAttributes("link").href;
                setLinkHref(String(href ?? ""));
                setLinkSelected(Boolean(href));
                setLinkError(false);
              }
            }}
          >
            <PopoverTrigger disableButtonEnhancement>
              <ToolbarButton aria-label={t("generatedEditor.link")}>
                {t("generatedEditor.link")}
              </ToolbarButton>
            </PopoverTrigger>
            <PopoverSurface className="generated-link-popover">
              <Input
                aria-label={t("generatedEditor.linkUrl")}
                value={linkHref}
                onChange={(_, data) => setLinkHref(data.value)}
              />
              <Button
                onClick={() => {
                  try {
                    toRichNode({
                      type: "text",
                      text: "link",
                      marks: [{ type: "link", attrs: { href: linkHref } }],
                    });
                    editor
                      ?.chain()
                      .focus()
                      .extendMarkRange("link")
                      .setLink({ href: linkHref })
                      .run();
                    setLinkError(false);
                  } catch {
                    setLinkError(true);
                  }
                }}
              >
                {t("generatedEditor.link")}
              </Button>
              {linkSelected && (
                <Button
                  onClick={() =>
                    editor
                      ?.chain()
                      .focus()
                      .extendMarkRange("link")
                      .unsetLink()
                      .run()
                  }
                >
                  {t("generatedEditor.unlink")}
                </Button>
              )}
              {linkError && (
                <span role="alert">{t("generatedEditor.invalidLink")}</span>
              )}
            </PopoverSurface>
          </Popover>
          <ToolbarToggleButton
            name="format"
            value="bullets"
            onClick={() => editor?.chain().focus().toggleBulletList().run()}
          >
            {t("generatedEditor.bullets")}
          </ToolbarToggleButton>
          <ToolbarToggleButton
            name="format"
            value="numbered"
            onClick={() => editor?.chain().focus().toggleOrderedList().run()}
          >
            {t("generatedEditor.numbered")}
          </ToolbarToggleButton>
          <ToolbarToggleButton
            name="format"
            value="codeBlock"
            onClick={() => editor?.chain().focus().toggleCodeBlock().run()}
          >
            {t("generatedEditor.codeBlock")}
          </ToolbarToggleButton>
          <ToolbarButton
            onClick={() =>
              editor
                ?.chain()
                .focus()
                .insertTable({ rows: 2, cols: 2, withHeaderRow: true })
                .run()
            }
          >
            {t("generatedEditor.table")}
          </ToolbarButton>
          <ToolbarButton onClick={() => editor?.chain().focus().undo().run()}>
            {t("generatedEditor.undo")}
          </ToolbarButton>
          <ToolbarButton onClick={() => editor?.chain().focus().redo().run()}>
            {t("generatedEditor.redo")}
          </ToolbarButton>
        </Toolbar>
      )}
      {!unsupported && <EditorContent editor={editor} />}
    </div>
  );
}
