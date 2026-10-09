import { Extension, Node, getSchema } from "@tiptap/core";
import StarterKit from "@tiptap/starter-kit";
import { TableKit } from "@tiptap/extension-table";
import { DOMSerializer } from "@tiptap/pm/model";

// Product-specific media representation only; Tiptap/ProseMirror own editing,
// DOM parsing, escaping, marks, lists and tables. No platform selectors here.
const PublicationImage = Node.create({
  name: "publicationImage",
  group: "block",
  atom: true,
  draggable: true,
  addAttributes() {
    return {
      src: { default: null },
      alt: { default: "" },
      caption: { default: "" },
      width: { default: null },
      height: { default: null },
    };
  },
  parseHTML() {
    return [
      {
        tag: "figure",
        getAttrs(element) {
          const image = element.querySelector(":scope > img");
          if (!image) return false;
          return {
            src: image.getAttribute("src"),
            alt: image.getAttribute("alt") ?? "",
            caption:
              element.querySelector(":scope > figcaption")?.textContent ?? "",
            width: Number(image.getAttribute("width")) || null,
            height: Number(image.getAttribute("height")) || null,
          };
        },
      },
    ];
  },
  renderHTML({ node }) {
    const { src, alt, caption, width, height } = node.attrs;
    return [
      "figure",
      {},
      ["img", { src, alt, width, height }],
      ...(caption ? [["figcaption", {}, caption]] : []),
    ];
  },
});

// Link title is part of the frozen contract but not a default Tiptap attr.
const PublicationLinkTitle = Extension.create({
  name: "publicationLinkTitle",
  addGlobalAttributes() {
    return [
      {
        types: ["link"],
        attributes: { title: { default: null } },
      },
    ];
  },
});

export function richPublicationExtensions() {
  return [
    StarterKit.configure({
      blockquote: false,
      horizontalRule: false,
      trailingNode: false,
      link: {
        openOnClick: false,
        autolink: false,
        HTMLAttributes: { target: null, rel: null },
      },
    }),
    TableKit,
    PublicationImage,
    PublicationLinkTitle,
  ];
}

/**
 * Serialize in a controlled DOM (e.g. an isolated local browser page). This
 * must not be mistaken for inserting into a live platform editor: even an
 * autosaving editor is an external write and needs the existing send grant.
 */
export function serializeRichEditorDocument(content, document) {
  const schema = getSchema(richPublicationExtensions());
  const node = schema.nodeFromJSON(content);
  node.check();
  const container = document.createElement("div");
  container.appendChild(
    DOMSerializer.fromSchema(schema).serializeFragment(node.content, {
      document,
    }),
  );
  return container.innerHTML;
}
