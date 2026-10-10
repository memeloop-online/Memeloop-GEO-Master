import assert from "node:assert/strict";
import { test } from "node:test";
import { richPublicationToEditor } from "../src/rich-editor-bridge.mjs";
import { richEditorFixture } from "./fixtures/rich-editor.mjs";

test("exact frozen media mapping preserves separate occurrences without mutating payload", () => {
  const { payload, mapping } = richEditorFixture();
  const before = JSON.stringify(payload);
  const result = richPublicationToEditor(payload, mapping);
  const images = result.content.content.filter(
    (node) => node.type === "publicationImage",
  );
  assert.equal(images.length, 2);
  assert.equal(images[0].attrs.src, images[1].attrs.src);
  assert.notEqual(images[0].attrs.alt, images[1].attrs.alt);
  assert.notEqual(images[0].attrs.caption, images[1].attrs.caption);
  assert.equal(JSON.stringify(payload), before);
  assert.ok(!JSON.stringify(result).includes(mapping[0].object.object_id));
  assert.ok(!JSON.stringify(result).includes(mapping[0].binding_id));
  assert.equal(
    result.content.content[4].content[0].content[0].attrs.align,
    "center",
  );
});

test("rejects missing, extraneous, duplicate, stale and wrong-binding mappings", () => {
  const { payload, mapping } = richEditorFixture();
  for (const invalid of [
    [],
    [...mapping, ...mapping],
    [{ ...mapping[0], extra: true }],
    [{ ...mapping[0], binding_id: "00000004-1111-4111-8111-111111111111" }],
    [{ ...mapping[0], object: { ...mapping[0].object, object_version: 3 } }],
    [
      {
        ...mapping[0],
        object: { ...mapping[0].object, sha256: "0".repeat(64) },
      },
    ],
    [
      {
        ...mapping[0],
        object: {
          ...mapping[0].object,
          object_id: "00000004-1111-4111-8111-111111111111",
        },
      },
    ],
  ])
    assert.throws(() => richPublicationToEditor(payload, invalid), {
      code: "editor_media_mapping_mismatch",
    });
  const changed = structuredClone(payload);
  changed.media[1].alt = "not the frozen occurrence";
  assert.throws(() => richPublicationToEditor(changed, mapping), {
    code: "media_manifest_mismatch",
  });
});

test("rejects active, credential-bearing, path and remote plaintext media sources", () => {
  const { payload, mapping } = richEditorFixture();
  for (const src of [
    "javascript:alert(1)",
    "data:image/png;base64,AA==",
    "file:///tmp/image",
    "//media.example.test/i.png",
    "https://user:pass@media.example.test/i",
    "http://media.example.test/i",
    "https://media.example.test/a\nb",
  ])
    assert.throws(
      () => richPublicationToEditor(payload, [{ ...mapping[0], src }]),
      { code: "editor_media_mapping_mismatch" },
    );
});

test("mapping order is independent of occurrence order but duplicate entries cannot hide a missing asset", () => {
  const { payload, mapping } = richEditorFixture();
  const secondObject = { ...mapping[0].object, object_version: 3 };
  payload.media[1].object = secondObject;
  const occurrence = payload.document.blocks.at(-1).rich.node.attrs;
  occurrence.object_version = secondObject.object_version;
  const secondMapping = {
    ...mapping[0],
    object: secondObject,
    src: "https://media.example.test/second.png",
  };
  const result = richPublicationToEditor(payload, [secondMapping, ...mapping]);
  const images = result.content.content.filter(
    (node) => node.type === "publicationImage",
  );
  assert.deepEqual(
    images.map((node) => node.attrs.src),
    [mapping[0].src, secondMapping.src],
  );
  assert.throws(
    () => richPublicationToEditor(payload, [...mapping, ...mapping]),
    { code: "editor_media_mapping_mismatch" },
  );
});

test("unsupported structure fails before Tiptap can silently discard it", () => {
  const { payload, mapping } = richEditorFixture();
  payload.document.blocks[0].rich.node.type = "unsupportedWidget";
  assert.throws(() => richPublicationToEditor(payload, mapping), {
    code: "unsupported_rich_node",
  });
});

test("legacy blocks inside rich payload retain ordered content", () => {
  const { payload } = richEditorFixture();
  payload.media = [];
  payload.document.blocks = [
    {
      block_id: "00000001-2222-4222-8222-222222222222",
      kind: "heading",
      text: "Heading",
    },
    {
      block_id: "00000002-2222-4222-8222-222222222222",
      kind: "list",
      text: "List introduction",
      items: ["one", "two"],
    },
    {
      block_id: "00000003-2222-4222-8222-222222222222",
      kind: "paragraph",
      text: "tail",
    },
  ];
  const { content } = richPublicationToEditor(payload, []);
  assert.deepEqual(
    content.content.map((node) => node.type),
    ["heading", "paragraph", "bulletList", "paragraph"],
  );
  assert.equal(content.content[1].content[0].text, "List introduction");
  assert.equal(content.content[2].content[1].content[0].content[0].text, "two");
});
