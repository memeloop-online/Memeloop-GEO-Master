// Mirrors the bounded schema-2 storage adapter. Rust remains authoritative for
// tree placement, aggregate limits, evidence, media rights and revision CAS.
const text = { type: "string", maxLength: 1_000_000 };
const uuid = { type: "string", format: "uuid" };
const node = { $ref: "#/$defs/richNode" };
const children = { type: "array", maxItems: 10_000, items: node };
const object = (properties, required = Object.keys(properties)) => ({
  type: "object",
  additionalProperties: false,
  required,
  properties,
});
const typed = (type, properties = {}, required = Object.keys(properties)) =>
  object({ type: { const: type }, ...properties }, ["type", ...required]);
const mark = {
  anyOf: [
    ...["bold", "italic", "strike", "underline", "code"].map((type) =>
      typed(type),
    ),
    typed("link", {
      attrs: object(
        {
          href: { type: "string", minLength: 1, maxLength: 2048 },
          title: { ...text, type: ["string", "null"] },
        },
        ["href"],
      ),
    }),
  ],
};
const cellAttrs = object(
  {
    colspan: { type: "integer", minimum: 1, maximum: 32 },
    rowspan: { type: "integer", minimum: 1, maximum: 100 },
    colwidth: {
      type: ["array", "null"],
      maxItems: 32,
      items: { type: "integer", minimum: 1, maximum: 65535 },
    },
    textAlign: { enum: ["left", "center", "right", null] },
  },
  [],
);
const richNode = {
  anyOf: [
    typed("paragraph", { content: children }, []),
    typed(
      "heading",
      {
        attrs: object({ level: { type: "integer", minimum: 1, maximum: 6 } }),
        content: children,
      },
      ["attrs"],
    ),
    typed(
      "text",
      {
        text,
        marks: { type: "array", maxItems: 6, items: mark },
      },
      ["text"],
    ),
    typed("hardBreak"),
    ...["bulletList", "listItem", "table", "tableRow"].map((type) =>
      typed(type, { content: children }),
    ),
    typed("orderedList", {
      attrs: object({
        start: { type: "integer", minimum: 1, maximum: 4294967295 },
      }),
      content: children,
    }),
    typed(
      "codeBlock",
      {
        attrs: {
          anyOf: [
            object(
              { language: { type: ["string", "null"], maxLength: 64 } },
              [],
            ),
            { type: "null" },
          ],
        },
        content: children,
      },
      ["content"],
    ),
    ...["tableHeader", "tableCell"].map((type) =>
      typed(
        type,
        { attrs: { anyOf: [cellAttrs, { type: "null" }] }, content: children },
        ["content"],
      ),
    ),
    typed("media", {
      attrs: object({
        object_id: uuid,
        object_version: { type: "integer", minimum: 1 },
        sha256: { type: "string", pattern: "^[a-fA-F0-9]{64}$" },
        alt: { type: "string", minLength: 1, maxLength: 1000 },
        caption: { type: "string", maxLength: 1000 },
      }),
    }),
  ],
};

export const CONTENT_DOCUMENT_REVISE_SCHEMA = {
  ...object({
    execution_id: uuid,
    item_id: uuid,
    base_revision_id: uuid,
    document: object({
      schema_version: { type: "integer", const: 2 },
      title: { type: "string", minLength: 1, maxLength: 4096 },
      blocks: {
        type: "array",
        minItems: 1,
        maxItems: 10_000,
        items: object(
          {
            block_id: uuid,
            kind: { enum: ["heading", "paragraph", "list", "rich"] },
            text,
            citation_ids: { type: "array", maxItems: 10_000, items: uuid },
            items: { type: "array", maxItems: 10_000, items: text },
            rich: {
              anyOf: [
                object({ version: { type: "integer", const: 1 }, node }),
                { type: "null" },
              ],
            },
          },
          ["block_id", "kind", "text"],
        ),
      },
    }),
  }),
  $defs: { richNode },
};
