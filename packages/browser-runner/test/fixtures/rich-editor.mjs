import { createHash } from "node:crypto";

export const imageBytes = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aX1sAAAAASUVORK5CYII=",
  "base64",
);
const uuid = (n) => `${String(n).padStart(8, "0")}-1111-4111-8111-111111111111`;
const text = (value, marks) => ({
  type: "text",
  text: value,
  ...(marks ? { marks } : {}),
});
const paragraph = (value) => ({ type: "paragraph", content: [text(value)] });

/** Synthetic, platform-neutral fixture; no account or real publication data. */
export function richEditorFixture(
  src = "https://media.example.test/image.png",
) {
  const object = {
    object_id: uuid(1),
    object_version: 2,
    sha256: createHash("sha256").update(imageBytes).digest("hex"),
  };
  const image = (alt, caption) => ({
    type: "media",
    attrs: { ...object, alt, caption },
  });
  const nodes = [
    {
      type: "heading",
      attrs: { level: 3 },
      content: [text("中文 heading <safe>")],
    },
    {
      type: "paragraph",
      content: [
        ...["bold", "italic", "strike", "underline", "code"].map((type) =>
          text(`${type} `, [{ type }]),
        ),
        text("linked", [
          {
            type: "link",
            attrs: {
              href: "https://example.test/path?q=1&b=2",
              title: "Link <title>",
            },
          },
        ]),
        { type: "hardBreak" },
        text("<script>not executable</script>"),
      ],
    },
    {
      type: "orderedList",
      attrs: { start: 4 },
      content: [
        {
          type: "listItem",
          content: [
            paragraph("ordered"),
            {
              type: "bulletList",
              content: [
                {
                  type: "listItem",
                  content: [
                    paragraph("nested bullet"),
                    {
                      type: "orderedList",
                      attrs: { start: 8 },
                      content: [
                        {
                          type: "listItem",
                          content: [paragraph("deep ordered")],
                        },
                      ],
                    },
                  ],
                },
              ],
            },
          ],
        },
      ],
    },
    {
      type: "codeBlock",
      attrs: { language: "js" },
      content: [text("  if (a < 2) {\n    run();\n  }\n")],
    },
    {
      type: "table",
      content: [
        {
          type: "tableRow",
          content: [
            {
              type: "tableHeader",
              attrs: { colspan: 2, colwidth: [120, 130], textAlign: "center" },
              content: [paragraph("Merged header")],
            },
          ],
        },
        {
          type: "tableRow",
          content: [
            {
              type: "tableCell",
              attrs: { rowspan: 2, textAlign: "right" },
              content: [paragraph("Spanning cell")],
            },
            { type: "tableCell", content: [paragraph("Cell one")] },
          ],
        },
        {
          type: "tableRow",
          content: [{ type: "tableCell", content: [{ type: "paragraph" }] }],
        },
      ],
    },
    image("First <alt>", "First caption <not markup>"),
    paragraph("Between repeated occurrences"),
    image("第二张替代文字", "Second caption & distinct"),
  ];
  const payload = {
    schema_version: 2,
    format: "rich_markdown.v2",
    policy_version: "deterministic-rich-markdown-v2",
    content_revision_id: uuid(2),
    document: {
      schema_version: 2,
      title: "Synthetic rich publication",
      blocks: nodes.map((node, i) => ({
        block_id: uuid(i + 10),
        kind: "rich",
        text: "",
        rich: { version: 1, node },
      })),
    },
    media: nodes
      .filter((node) => node.type === "media")
      .map(({ attrs }) => ({
        binding_id: uuid(3),
        object: { ...object },
        media_type: "image/png",
        byte_len: imageBytes.length,
        width: 1,
        height: 1,
        role: "image",
        alt: attrs.alt,
        caption: attrs.caption,
      })),
  };
  return {
    payload,
    mapping: [{ binding_id: uuid(3), object: { ...object }, src }],
  };
}
