import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { before, test } from "node:test";
import fontkit from "@pdf-lib/fontkit";
import { createHash } from "node:crypto";
import fontClosure from "../src/pdf-font-subset-closure.json" with { type: "json" };
import { createPdfFontSubsetter } from "../src/pdf-font-subset.mjs";

const sample =
  "简体中文推荐报告 繁體臺灣香港 龘𠮷 ABCxyz0123456789 ,.!?：；（）";
const codepoints = [...sample].map((character) => character.codePointAt(0));
let source;
let subsetPdfFont;

before(async () => {
  // Reuse the approved, attributed repository font; no duplicate fixture binary.
  source = await readFile(
    new URL("../assets/fonts/NotoSansCJKsc-Regular.otf", import.meta.url),
  );
  const supplied = Buffer.from(source);
  subsetPdfFont = await createPdfFontSubsetter({ fontBytes: supplied });
  supplied.fill(0); // The factory owns a stable copy of the configured font.
});

test("retains supported Unicode glyph IDs, metrics and outlines in a small OTF", async () => {
  const bytes = await subsetPdfFont({ codepoints });
  assert.ok(bytes.length < source.length / 10);
  const original = fontkit.create(source);
  const actual = fontkit.create(bytes);
  for (const codepoint of new Set(codepoints)) {
    const expectedGlyph = original.glyphForCodePoint(codepoint);
    const actualGlyph = actual.glyphForCodePoint(codepoint);
    assert.notEqual(actualGlyph.id, 0);
    assert.equal(actualGlyph.id, expectedGlyph.id);
    assert.equal(actualGlyph.advanceWidth, expectedGlyph.advanceWidth);
    assert.deepEqual(actualGlyph.path.commands, expectedGlyph.path.commands);
  }
});

test("sorts and deduplicates codepoints without changing output bytes", async () => {
  const first = await subsetPdfFont({ codepoints });
  const reordered = await subsetPdfFont({
    codepoints: Uint32Array.from([...codepoints].reverse().concat(codepoints)),
  });
  assert.deepEqual(reordered, first);
});

test("preserves shaped ligature glyphs, positions and their ToUnicode cmap aliases", async () => {
  const text =
    "office verified finding insufficient efficient ff fi fl ffi ffl —— ——— ᄀᄃ ᄅᄆ";
  const original = fontkit.create(source);
  const actual = fontkit.create(
    await subsetPdfFont({
      codepoints: [...text].map((character) => character.codePointAt(0)),
    }),
  );
  const expectedRun = original.layout(text, { locl: false });
  const actualRun = actual.layout(text, { locl: false });
  assert.deepEqual(
    actualRun.glyphs.map((glyph) => glyph.id),
    expectedRun.glyphs.map((glyph) => glyph.id),
  );
  assert.deepEqual(actualRun.positions, expectedRun.positions);
  for (const codepoint of [
    0xfb00, 0xfb01, 0xfb02, 0xfb03, 0xfb04, 0x2e3a, 0x2e3b, 0x115a, 0xa968,
  ]) {
    assert.ok(actual.characterSet.includes(codepoint));
    assert.equal(
      actual.glyphForCodePoint(codepoint).id,
      original.glyphForCodePoint(codepoint).id,
    );
  }
});

test("retains the known font's complete encoded default GSUB alias metadata", async () => {
  assert.equal(
    createHash("sha256").update(source).digest("hex"),
    fontClosure.sourceFontSha256,
  );
  const aliases = fontClosure.codepointRanges.flatMap(([start, end]) =>
    Array.from({ length: end - start + 1 }, (_, offset) => start + offset),
  );
  assert.equal(aliases.length, fontClosure.aliasCount);
  assert.equal(aliases.length, 129);
  const original = fontkit.create(source);
  const actual = fontkit.create(await subsetPdfFont({ codepoints: [65] }));
  for (const codepoint of aliases) {
    assert.ok(original.characterSet.includes(codepoint));
    assert.ok(actual.characterSet.includes(codepoint));
    assert.equal(
      actual.glyphForCodePoint(codepoint).id,
      original.glyphForCodePoint(codepoint).id,
    );
  }
});

test("supports arbitrary characters covered by the font, beyond the report sample", async () => {
  const arbitrary = [..."日本語한국어éΩЖ"].map((character) =>
    character.codePointAt(0),
  );
  const actual = fontkit.create(await subsetPdfFont({ codepoints: arbitrary }));
  const original = fontkit.create(source);
  for (const codepoint of arbitrary) {
    assert.equal(
      actual.glyphForCodePoint(codepoint).id,
      original.glyphForCodePoint(codepoint).id,
    );
  }
});

test("unsupported characters do not block supported text or invent a glyph", async () => {
  const bytes = await subsetPdfFont({ codepoints: [65, 0x10ffff] });
  const actual = fontkit.create(bytes);
  assert.equal(
    actual.glyphForCodePoint(65).id,
    fontkit.create(source).glyphForCodePoint(65).id,
  );
  assert.equal(actual.glyphForCodePoint(0x10ffff).id, 0);
});

test("rejects malformed Unicode scalar input", async () => {
  for (const invalid of [
    undefined,
    "中文",
    [-1],
    [0xd800],
    [0xdfff],
    [0x110000],
    [1.5],
    [NaN],
    ["65"],
  ]) {
    await assert.rejects(subsetPdfFont({ codepoints: invalid }), TypeError);
  }
});

test("empty input still produces a readable font", async () => {
  const actual = fontkit.create(await subsetPdfFont({ codepoints: [] }));
  assert.ok(actual.numGlyphs >= 1);
});

test("cancels before work and while queued, then remains reusable", async () => {
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(
    subsetPdfFont({ codepoints, signal: controller.signal }),
    { name: "AbortError" },
  );
  const queued = new AbortController();
  const pending = subsetPdfFont({ codepoints, signal: queued.signal });
  queued.abort();
  await assert.rejects(pending, { name: "AbortError" });
  const expected = await subsetPdfFont({ codepoints });
  const [first, second] = await Promise.all([
    subsetPdfFont({ codepoints }),
    subsetPdfFont({ codepoints }),
  ]);
  assert.deepEqual(first, expected);
  assert.deepEqual(second, expected);
  first.fill(0);
  assert.deepEqual(await subsetPdfFont({ codepoints }), expected);
});

test("rejects invalid configured font bytes", async () => {
  await assert.rejects(
    createPdfFontSubsetter({ fontBytes: new Uint8Array() }),
    TypeError,
  );
  await assert.rejects(
    createPdfFontSubsetter({ fontBytes: Buffer.from("invalid font") }),
    /approved PDF font/,
  );
});
