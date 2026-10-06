import { useEffect, useState } from "react";
import { Button } from "@fluentui/react-components";
import { Extension, type JSONContent } from "@tiptap/core";
import { Plugin } from "@tiptap/pm/state";
import { EditorContent, useEditor } from "@tiptap/react";
import StarterKit from "@tiptap/starter-kit";
import type { ContentBlock, StructuredDocument } from "../api/content";
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
              !["heading", "paragraph", "bulletList"].includes(node.type.name)
            )
              return;
            const inherited =
              node.type.name === "bulletList"
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
        types: ["heading", "paragraph", "bulletList"],
        attributes: {
          geoBlockId: {
            default: null,
            parseHTML: (element) => element.getAttribute("data-geo-block-id"),
            renderHTML: (attributes) =>
              attributes.geoBlockId
                ? { "data-geo-block-id": attributes.geoBlockId }
                : {},
          },
          geoCitations: {
            default: [],
            parseHTML: (element) => {
              try {
                return JSON.parse(
                  element.getAttribute("data-geo-citations") ?? "[]",
                );
              } catch {
                return [];
              }
            },
            renderHTML: (attributes) => ({
              "data-geo-citations": JSON.stringify(
                attributes.geoCitations ?? [],
              ),
            }),
          },
          geoCaptionFor: {
            default: null,
            parseHTML: (element) =>
              element.getAttribute("data-geo-caption-for"),
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
    bold: false,
    code: false,
    codeBlock: false,
    dropcursor: false,
    gapcursor: false,
    hardBreak: false,
    horizontalRule: false,
    italic: false,
    link: false,
    orderedList: false,
    strike: false,
    trailingNode: false,
    underline: false,
    heading: { levels: [2] },
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
    if (node.type === "bulletList") {
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
          originalList?.kind === "list"
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
        next?.type === "bulletList" &&
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
    if (node.type === "heading" || node.type === "paragraph")
      blocks.push({
        ...attrs,
        kind: node.type,
        text: nodeText(node),
        items: [],
      });
    else if (node.type === "bulletList")
      blocks.push({ ...attrs, kind: "list", text: "", items: listItems(node) });
    else throw new Error("正文包含当前版本不支持的结构，请移除后保存。");
  }
  if (
    !blocks.length ||
    blocks.some(
      (block) =>
        (block.kind === "list" && !block.items.length) ||
        (!block.text.trim() && !block.items.some((item) => item.trim())),
    )
  )
    throw new Error("正文至少需要一个非空内容块；空白块尚不能保存。");
  return { title: previous.title, blocks };
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
  readonly: boolean;
  onChange: (document: StructuredDocument | null, error: string | null) => void;
}

export function StructuredContentEditor({
  document,
  readonly,
  onChange,
}: Props) {
  const [original] = useState(document);
  const editor = useEditor({
    immediatelyRender: false,
    extensions,
    editable: !readonly,
    content: documentToEditor(document),
    editorProps: {
      attributes: {
        "aria-label": "结构化正文",
        role: "textbox",
        class: "structured-content-input",
      },
      handlePaste: (_view, event) => {
        const pasted = event.clipboardData?.getData("text/plain");
        if (pasted === undefined) return false;
        event.preventDefault();
        // Untrusted HTML, marks, images and table structure have no persisted
        // domain representation. Never accept their clipboard HTML payload.
        if (pasted)
          editor?.commands.insertContent({ type: "text", text: pasted });
        return true;
      },
    },
    onUpdate: ({ editor: current }) => {
      try {
        onChange(editorToDocument(current.getJSON(), original), null);
      } catch (error) {
        onChange(null, (error as Error).message);
      }
    },
  });

  useEffect(() => {
    editor?.setEditable(!readonly);
  }, [editor, readonly]);

  const toParagraph = () => {
    if (!editor) return;
    const selection = editor.state.selection.$from;
    let depth = selection.depth;
    while (depth > 0 && selection.node(depth).type.name !== "bulletList")
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
    editor.chain().focus().toggleBulletList().setParagraph().run();
  };

  return (
    <div className="structured-content-editor">
      {!readonly && (
        <div
          className="structured-content-toolbar"
          role="toolbar"
          aria-label="正文格式"
        >
          <Button size="small" onClick={toParagraph}>
            段落
          </Button>
          <Button
            size="small"
            onClick={() =>
              editor?.chain().focus().toggleHeading({ level: 2 }).run()
            }
          >
            小标题
          </Button>
          <Button
            size="small"
            onClick={() => editor?.chain().focus().toggleBulletList().run()}
          >
            列表
          </Button>
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
