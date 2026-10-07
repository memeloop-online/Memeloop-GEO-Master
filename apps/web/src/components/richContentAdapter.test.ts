import { describe, expect, it } from "vitest";
import type { JSONContent } from "@tiptap/core";
import type { StructuredDocument } from "../api/content";
import { documentToEditor, editorToDocument } from "./StructuredContentEditor";
import { fromRichNode, toRichNode } from "./richContentAdapter";

const prior: StructuredDocument = {
  title: "Guide",
  blocks: [
    {
      block_id: "block-1",
      kind: "paragraph",
      text: "Before",
      citation_ids: ["evidence-1"],
      items: [],
    },
  ],
};

describe("generated rich content adapter", () => {
  it("roundtrips a verified media block with its caption, identity and evidence envelope", () => {
    const reference = {
      object_id: "1e47ee2e-534a-4695-a998-46a32639d0b2",
      object_version: 2,
      sha256: "a".repeat(64),
      alt: "A sample diagram",
      caption: "Figure one",
    };
    const media: StructuredDocument = {
      title: "Guide",
      schema_version: 2,
      blocks: [
        {
          block_id: "media-block",
          kind: "rich",
          citation_ids: ["evidence-1"],
          text: "",
          items: [],
          rich: { version: 1, node: { type: "media", attrs: reference } },
        },
      ],
    };
    expect(editorToDocument(documentToEditor(media), media)).toEqual(media);
    const inserted = editorToDocument(
      {
        ...documentToEditor(prior),
        content: [
          ...documentToEditor(prior).content!,
          { type: "media", attrs: { ...reference, geoCitations: [] } },
        ],
      },
      prior,
    );
    expect(inserted.blocks[0]).toEqual(prior.blocks[0]);
    expect(inserted.blocks[1]).toMatchObject({
      kind: "rich",
      citation_ids: [],
      rich: { node: { type: "media", attrs: reference } },
    });
    expect(inserted.blocks[1].block_id).not.toBe(prior.blocks[0].block_id);
    expect(() =>
      toRichNode({
        type: "media",
        attrs: { ...reference, src: "https://example.test/image.png" },
      }),
    ).toThrow();
    expect(() =>
      toRichNode({ type: "media", attrs: { ...reference, alt: "" } }),
    ).toThrow();
    expect(() =>
      fromRichNode({
        type: "media",
        attrs: { ...reference, caption: undefined },
      }),
    ).toThrow();
  });

  it("keeps legacy documents exactly unchanged when opened and saved without a rich edit", () => {
    expect(editorToDocument(documentToEditor(prior), prior)).toEqual(prior);
  });

  it("upgrades only an edited block and keeps its identity and citations", () => {
    const edited = documentToEditor(prior);
    edited.content![0].content = [
      { type: "text", text: "Before", marks: [{ type: "bold" }] },
      { type: "hardBreak" },
      { type: "text", text: "After", marks: [{ type: "italic" }] },
    ];
    const result = editorToDocument(edited, prior);
    expect(result.schema_version).toBe(2);
    expect(result.blocks[0]).toEqual({
      block_id: "block-1",
      citation_ids: ["evidence-1"],
      kind: "rich",
      text: "",
      items: [],
      rich: {
        version: 1,
        node: {
          type: "paragraph",
          content: edited.content![0].content,
        },
      },
    });
    expect(editorToDocument(documentToEditor(result), result)).toEqual(result);
  });

  it("does not flatten styled list captions or nested list items", () => {
    const original: StructuredDocument = {
      title: "Evidence list",
      blocks: [
        {
          block_id: "list-1",
          kind: "list",
          text: "Caption",
          citation_ids: ["evidence-1"],
          items: ["First"],
        },
      ],
    };
    const edited = documentToEditor(original);
    edited.content![0].content![0].marks = [{ type: "bold" }];
    edited.content![1].content![0].content!.push({
      type: "bulletList",
      content: [
        {
          type: "listItem",
          content: [
            {
              type: "paragraph",
              content: [{ type: "text", text: "Nested" }],
            },
          ],
        },
      ],
    });
    const result = editorToDocument(edited, original);
    expect(result.schema_version).toBe(2);
    expect(result.blocks).toHaveLength(2);
    expect(result.blocks[0]).toMatchObject({
      block_id: "list-1",
      citation_ids: ["evidence-1"],
      kind: "rich",
      rich: {
        node: { type: "paragraph", content: [{ marks: [{ type: "bold" }] }] },
      },
    });
    expect(result.blocks[1]).toMatchObject({
      citation_ids: ["evidence-1"],
      kind: "rich",
      rich: {
        node: {
          type: "bulletList",
          content: [
            {
              content: [{ type: "paragraph" }, { type: "bulletList" }],
            },
          ],
        },
      },
    });
    expect(result.blocks[1].block_id).not.toBe("list-1");
  });

  it("retains legal citations when a rich block's evidence envelope changes", () => {
    const rich = editorToDocument(
      {
        type: "doc",
        content: [
          {
            type: "paragraph",
            attrs: { geoBlockId: "block-1", geoCitations: ["evidence-1"] },
            content: [
              { type: "text", text: "Formatted", marks: [{ type: "bold" }] },
            ],
          },
        ],
      },
      prior,
    );
    const withAnotherCitation: StructuredDocument = {
      ...rich,
      blocks: [
        ...rich.blocks,
        {
          block_id: "block-2",
          kind: "paragraph",
          text: "Other",
          items: [],
          citation_ids: ["evidence-2"],
        },
      ],
    };
    const edited = documentToEditor(withAnotherCitation);
    edited.content![0].attrs!.geoCitations = ["evidence-1", "evidence-2"];
    const result = editorToDocument(edited, withAnotherCitation);
    expect(result.blocks[0].citation_ids).toEqual(["evidence-1", "evidence-2"]);
    expect(result.blocks[0].rich).toEqual(withAnotherCitation.blocks[0].rich);
  });

  it("preserves nested numbered lists, code language and table cell layout", () => {
    const trees: JSONContent[] = [
      {
        type: "orderedList",
        attrs: { start: 4 },
        content: [
          {
            type: "listItem",
            content: [
              { type: "paragraph", content: [{ type: "text", text: "First" }] },
              {
                type: "bulletList",
                content: [
                  {
                    type: "listItem",
                    content: [
                      {
                        type: "paragraph",
                        content: [{ type: "text", text: "Nested" }],
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
        attrs: { language: "rust" },
        content: [{ type: "text", text: "a * b" }],
      },
      {
        type: "table",
        content: [
          {
            type: "tableRow",
            content: [
              {
                type: "tableHeader",
                attrs: {
                  colspan: 2,
                  rowspan: 1,
                  colwidth: [100, 120],
                  textAlign: "center",
                },
                content: [
                  {
                    type: "paragraph",
                    content: [{ type: "text", text: "Header" }],
                  },
                ],
              },
            ],
          },
          {
            type: "tableRow",
            content: [
              {
                type: "tableCell",
                attrs: {
                  colspan: 1,
                  rowspan: 1,
                  colwidth: null,
                  textAlign: null,
                },
                content: [
                  {
                    type: "paragraph",
                    content: [
                      {
                        type: "text",
                        text: "One",
                        marks: [{ type: "underline" }],
                      },
                    ],
                  },
                ],
              },
              {
                type: "tableCell",
                attrs: {
                  colspan: 1,
                  rowspan: 1,
                  colwidth: null,
                  textAlign: null,
                },
                content: [
                  {
                    type: "paragraph",
                    content: [{ type: "text", text: "Two" }],
                  },
                ],
              },
            ],
          },
        ],
      },
    ];
    const document: StructuredDocument = {
      title: "Rich",
      schema_version: 2,
      blocks: trees.map((node, index) => ({
        block_id: `block-${index}`,
        kind: "rich" as const,
        citation_ids: ["evidence-1"],
        text: "",
        items: [],
        rich: { version: 1 as const, node: toRichNode(node) },
      })),
    };
    expect(editorToDocument(documentToEditor(document), document)).toEqual(
      document,
    );
  });

  it("rejects unsafe links and unknown structure without text fallback", () => {
    expect(() =>
      toRichNode({
        type: "paragraph",
        content: [
          {
            type: "text",
            text: "click",
            marks: [{ type: "link", attrs: { href: "javascript:alert(1)" } }],
          },
        ],
      }),
    ).toThrow();
    expect(() =>
      fromRichNode({
        type: "paragraph",
        attrs: { unexpected: true },
        content: [],
      }),
    ).toThrow();
    expect(() =>
      fromRichNode({
        type: "paragraph",
        content: [
          {
            type: "text",
            text: "value",
            marks: [
              {
                type: "link",
                attrs: { href: "https://example.test", target: "_blank" },
              },
            ],
          },
        ],
      }),
    ).toThrow();
    expect(() =>
      documentToEditor({
        ...prior,
        schema_version: 2,
        blocks: [
          {
            ...prior.blocks[0],
            kind: "rich",
            text: "",
            rich: {
              version: 1,
              node: { type: "media", attrs: { object_id: "some-id" } },
            },
          },
        ],
      }),
    ).toThrow();
  });
});
