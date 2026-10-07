import type { JSONContent } from "@tiptap/core";
import type { RichNode } from "../api/content";

const allowedMarks = new Set([
  "bold",
  "italic",
  "strike",
  "underline",
  "code",
  "link",
]);
const safeLink = (href: string) => {
  if (
    !href ||
    href.length > 2048 ||
    /[\s\\\u0000-\u001f\u007f]/u.test(href) ||
    href.startsWith("//")
  )
    return false;
  try {
    const url = new URL(href);
    return (
      (["http:", "https:"].includes(url.protocol) &&
        !!url.hostname &&
        !url.username &&
        !url.password) ||
      (url.protocol === "mailto:" && !!url.pathname)
    );
  } catch {
    return !/^[a-z][\w+.-]*:/i.test(href);
  }
};

function keys(value: object, allowed: string[]) {
  if (Object.keys(value).some((key) => !allowed.includes(key)))
    throw new Error(
      "Content contains formatting this editor cannot safely save.",
    );
}

function attrs(node: JSONContent, names: string[]) {
  const value = node.attrs ?? {};
  keys(value, [...names, "geoBlockId", "geoCitations", "geoCaptionFor"]);
  return value;
}

/**
 * Only persist nodes shared by the editor and the domain schema. Tiptap adds
 * defaults (notably link target/rel and table cells); do not persist those as
 * unknown domain fields or import HTML-supplied evidence metadata.
 */
export function toRichNode(node: JSONContent, depth = 0): RichNode {
  if (depth > 24) throw new Error("Content is too deeply nested to save.");
  keys(node, ["type", "attrs", "content", "text", "marks"]);
  const children = (types: string[]) => {
    if (!node.content?.length)
      throw new Error("Content contains an empty list or table.");
    if (node.content.some((child) => !types.includes(child.type ?? "")))
      throw new Error("Content contains an unsupported nested structure.");
    return node.content.map((child) => toRichNode(child, depth + 1));
  };
  const inline = () =>
    node.content?.map((child) => {
      if (!["text", "hardBreak"].includes(child.type ?? ""))
        throw new Error("Content contains unsupported inline formatting.");
      return toRichNode(child, depth + 1);
    });
  switch (node.type) {
    case "paragraph":
      attrs(node, []);
      return {
        type: "paragraph",
        ...(inline()?.length ? { content: inline() } : {}),
      };
    case "heading": {
      const value = attrs(node, ["level"]);
      if (
        !Number.isInteger(value.level) ||
        Number(value.level) < 1 ||
        Number(value.level) > 6
      )
        throw new Error("Unsupported heading level.");
      return {
        type: "heading",
        attrs: { level: value.level },
        ...(inline()?.length ? { content: inline() } : {}),
      };
    }
    case "text": {
      attrs(node, []);
      if (!node.text) throw new Error("Content contains empty text.");
      const seen = new Set<string>();
      const marks = (node.marks ?? []).map((mark) => {
        if (!mark.type || !allowedMarks.has(mark.type) || seen.has(mark.type))
          throw new Error("Content contains an unsupported text style.");
        seen.add(mark.type);
        if (mark.type !== "link") {
          keys(mark, ["type", "attrs"]);
          keys(mark.attrs ?? {}, []);
          return { type: mark.type };
        }
        keys(mark, ["type", "attrs"]);
        const link = mark.attrs ?? {};
        keys(link, ["href", "title", "target", "rel", "class"]);
        if (typeof link.href !== "string" || !safeLink(link.href))
          throw new Error("This link address is not supported.");
        if (
          link.title != null &&
          (typeof link.title !== "string" ||
            link.title.length > 1024 ||
            /[\u0000-\u001f\u007f]/u.test(link.title))
        )
          throw new Error("This link title is not supported.");
        return {
          type: "link",
          attrs: {
            href: link.href,
            ...(link.title ? { title: link.title } : {}),
          },
        };
      });
      if (seen.has("code") && seen.size > 1)
        throw new Error("Inline code cannot have other text styles.");
      return {
        type: "text",
        text: node.text,
        ...(marks.length ? { marks } : {}),
      };
    }
    case "hardBreak":
      attrs(node, []);
      return { type: "hardBreak" };
    case "bulletList":
      attrs(node, ["tight"]);
      return { type: "bulletList", content: children(["listItem"]) };
    case "orderedList": {
      const value = attrs(node, ["start", "tight"]);
      const start = value.start ?? 1;
      if (!Number.isInteger(start) || Number(start) < 1)
        throw new Error("Invalid starting number.");
      return {
        type: "orderedList",
        attrs: { start },
        content: children(["listItem"]),
      };
    }
    case "listItem":
      attrs(node, []);
      if (node.content?.[0]?.type !== "paragraph")
        throw new Error("A list item must begin with a paragraph.");
      return {
        type: "listItem",
        content: children(["paragraph", "bulletList", "orderedList"]),
      };
    case "codeBlock": {
      const value = attrs(node, ["language"]);
      if (
        value.language != null &&
        (typeof value.language !== "string" ||
          !/^[\w+#-]{0,64}$/u.test(value.language))
      )
        throw new Error("Unsupported code language.");
      if (
        node.content?.some(
          (child) => child.type !== "text" || child.marks?.length,
        )
      )
        throw new Error("Code blocks cannot contain formatted text.");
      return {
        type: "codeBlock",
        attrs: { language: value.language ?? null },
        content: (node.content ?? []).map((child) =>
          toRichNode(child, depth + 1),
        ),
      };
    }
    case "table":
      attrs(node, []);
      if ((node.content?.length ?? 0) > 100)
        throw new Error("The table has too many rows.");
      return { type: "table", content: children(["tableRow"]) };
    case "tableRow":
      attrs(node, []);
      if ((node.content?.length ?? 0) > 32)
        throw new Error("The table has too many columns.");
      return {
        type: "tableRow",
        content: children(["tableCell", "tableHeader"]),
      };
    case "tableCell":
    case "tableHeader": {
      const value = attrs(node, [
        "colspan",
        "rowspan",
        "colwidth",
        "textAlign",
        "align",
      ]);
      const colspan = value.colspan ?? 1;
      const rowspan = value.rowspan ?? 1;
      const colwidth = value.colwidth ?? null;
      const textAlign = value.textAlign ?? value.align ?? null;
      if (
        value.textAlign != null &&
        value.align != null &&
        value.textAlign !== value.align
      )
        throw new Error("Conflicting table alignment.");
      if (
        !Number.isInteger(colspan) ||
        Number(colspan) < 1 ||
        Number(colspan) > 32 ||
        !Number.isInteger(rowspan) ||
        Number(rowspan) < 1 ||
        Number(rowspan) > 100 ||
        (colwidth !== null &&
          (!Array.isArray(colwidth) ||
            colwidth.length !== colspan ||
            colwidth.some(
              (width) => !Number.isInteger(width) || width <= 0,
            ))) ||
        (textAlign !== null &&
          !["left", "center", "right"].includes(String(textAlign)))
      )
        throw new Error("Invalid table cell layout.");
      return {
        type: node.type,
        attrs: {
          colspan,
          rowspan,
          ...(colwidth ? { colwidth } : {}),
          ...(textAlign ? { textAlign } : {}),
        },
        content: children(["paragraph"]),
      };
    }
    default:
      throw new Error(
        "Content contains a structure this editor cannot safely save.",
      );
  }
}

export function fromRichNode(node: RichNode): JSONContent {
  // Validate *before* Tiptap parses the JSON: Tiptap silently drops unknown
  // nodes, marks and attributes when constructing a ProseMirror document.
  const inspect = (entry: RichNode): void => {
    if (Object.keys(entry.attrs ?? {}).some((key) => key.startsWith("geo")))
      throw new Error("Content contains unrecognized evidence metadata.");
    if (
      entry.marks?.some(
        (mark) =>
          mark.type === "link" &&
          Object.keys(mark.attrs ?? {}).some(
            (key) => !["href", "title"].includes(key),
          ),
      )
    )
      throw new Error("Content contains unrecognized link attributes.");
    entry.content?.forEach(inspect);
  };
  inspect(node);
  const verified = toRichNode(node);
  const forEditor = (entry: RichNode): JSONContent => ({
    ...entry,
    ...(entry.type === "tableCell" || entry.type === "tableHeader"
      ? {
          attrs: {
            ...entry.attrs,
            align: entry.attrs?.textAlign ?? null,
            textAlign: undefined,
          },
        }
      : {}),
    ...(entry.content ? { content: entry.content.map(forEditor) } : {}),
  });
  return forEditor(verified);
}

export function isSimpleLegacy(node: JSONContent): boolean {
  if (node.type === "paragraph" || node.type === "heading")
    return (node.content ?? []).every(
      (child) => child.type === "text" && !child.marks?.length,
    );
  if (node.type === "bulletList")
    return (
      !!node.content?.length &&
      node.content.every(
        (item) =>
          item.type === "listItem" &&
          !!item.content?.length &&
          item.content.every(
            (paragraph) =>
              paragraph.type === "paragraph" &&
              (paragraph.content ?? []).every(
                (child) => child.type === "text" && !child.marks?.length,
              ),
          ),
      )
    );
  return false;
}
