import {
  RichPublicationError,
  validateRichPublicationPayload,
} from "./rich-publication.mjs";

const fail = () => {
  throw new RichPublicationError("editor_media_mapping_mismatch");
};
const key = (object) =>
  `${object.object_id.toLowerCase()}:${object.object_version}`;

function mediaSource(src) {
  if (
    typeof src !== "string" ||
    src.length > 8192 ||
    /[\s\\\u0000-\u001f\u007f]/u.test(src)
  )
    fail();
  let url;
  try {
    url = new URL(src);
  } catch {
    fail();
  }
  if (
    url.username ||
    url.password ||
    !(
      url.protocol === "https:" ||
      (url.protocol === "http:" &&
        ["127.0.0.1", "[::1]", "localhost"].includes(url.hostname))
    )
  )
    fail();
  return src;
}

/**
 * Pure adapter, not an upload or send grant. Call only with the exact frozen
 * payload and trusted upload results from the authorized execution. A URL is
 * a transient editor source, never evidence that the platform stored bytes.
 * Mapping is one entry per unique object version; document occurrences are
 * NOT deduplicated. Private object/binding IDs never enter the outgoing HTML.
 */
export function richPublicationToEditor(payload, uploadedMedia) {
  const manifest = validateRichPublicationPayload(payload);
  if (!Array.isArray(uploadedMedia) || uploadedMedia.length !== manifest.size)
    fail();
  const sources = new Map();
  for (const entry of uploadedMedia) {
    if (
      !entry ||
      Object.keys(entry).sort().join(",") !== "binding_id,object,src" ||
      !entry.object ||
      Object.keys(entry.object).sort().join(",") !==
        "object_id,object_version,sha256" ||
      typeof entry.object.object_id !== "string"
    )
      fail();
    const identity = key(entry.object);
    const expected = manifest.get(identity);
    if (
      !expected ||
      sources.has(identity) ||
      typeof entry.binding_id !== "string" ||
      expected.binding_id.toLowerCase() !== entry.binding_id.toLowerCase() ||
      expected.object.sha256 !== entry.object.sha256 ||
      expected.object.object_version !== entry.object.object_version
    )
      fail();
    sources.set(identity, mediaSource(entry.src));
  }
  const convert = (node) => {
    if (node.type === "media") {
      const item = manifest.get(key(node.attrs));
      return {
        type: "publicationImage",
        attrs: {
          src: sources.get(key(node.attrs)),
          alt: node.attrs.alt,
          caption: node.attrs.caption,
          width: item.width,
          height: item.height,
        },
      };
    }
    const copy = structuredClone(node);
    if (["tableCell", "tableHeader"].includes(node.type) && copy.attrs) {
      const { textAlign, ...rest } = copy.attrs;
      copy.attrs = { ...rest, ...(textAlign ? { align: textAlign } : {}) };
    }
    if (node.content) copy.content = node.content.map(convert);
    return copy;
  };
  const paragraph = (text) => ({
    type: "paragraph",
    ...(text ? { content: [{ type: "text", text }] } : {}),
  });
  return {
    title: payload.document.title,
    content: {
      type: "doc",
      content: payload.document.blocks.flatMap((block) => {
        if (block.kind === "rich") return convert(block.rich.node);
        if (block.kind === "heading")
          return {
            ...paragraph(block.text),
            type: "heading",
            attrs: { level: 2 },
          };
        if (block.kind === "list")
          return [
            ...(block.text ? [paragraph(block.text)] : []),
            {
              type: "bulletList",
              content: block.items.map((text) => ({
                type: "listItem",
                content: [paragraph(text)],
              })),
            },
          ];
        return paragraph(block.text);
      }),
    },
  };
}
