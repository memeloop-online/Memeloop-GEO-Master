import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { setImmediate } from "node:timers/promises";
import { createHash } from "node:crypto";
import fontClosure from "./pdf-font-subset-closure.json" with { type: "json" };

const require = createRequire(import.meta.url);
const HB_MEMORY_MODE_WRITABLE = 2;
const HB_SUBSET_FLAGS_RETAIN_GIDS = 0x00000002;
const HB_SUBSET_SETS_LAYOUT_FEATURE_TAG = 6;
// The fixed Noto font's default GSUB outputs have encoded Unicode aliases.
// pdf-lib builds ToUnicode from cmap, so keep these aliases as well as the
// GSUB-closed outlines; otherwise PDFBox extracts CFF private-use characters.
const PDF_GSUB_ALIASES = fontClosure.codepointRanges.flatMap(([start, end]) =>
  Array.from({ length: end - start + 1 }, (_, offset) => start + offset),
);

export class PdfFontSubsetInputError extends TypeError {}

function normalizeCodepoints(codepoints) {
  if (!Array.isArray(codepoints) && !(codepoints instanceof Uint32Array)) {
    throw new PdfFontSubsetInputError(
      "codepoints must be an array of Unicode scalar values",
    );
  }
  const unique = new Set();
  for (const codepoint of codepoints) {
    if (
      !Number.isInteger(codepoint) ||
      codepoint < 0 ||
      codepoint > 0x10ffff ||
      (codepoint >= 0xd800 && codepoint <= 0xdfff)
    ) {
      throw new PdfFontSubsetInputError(
        "codepoints must contain Unicode scalar values",
      );
    }
    unique.add(codepoint);
  }
  return [...unique].sort((left, right) => left - right);
}

/**
 * Bind a subsetter to the approved Noto font at startup. Requests cannot choose a
 * font or filesystem path. The caller owns font packaging and license notices.
 * Output remains compatible with pdf-lib's subset:false, features:{locl:false}.
 * HarfBuzz ignores unsupported codepoints. The report layout must retain its
 * existing missing-glyph fallback when collecting the final rendered charset.
 */
export async function createPdfFontSubsetter({ fontBytes }) {
  if (!(fontBytes instanceof Uint8Array) || fontBytes.byteLength === 0) {
    throw new TypeError("fontBytes must contain the configured font");
  }
  const source = Buffer.from(fontBytes);
  if (
    createHash("sha256").update(source).digest("hex") !==
    fontClosure.sourceFontSha256
  ) {
    throw new TypeError("fontBytes must match the approved PDF font");
  }
  const { instance } = await WebAssembly.instantiate(
    await readFile(require.resolve("harfbuzzjs/dist/harfbuzz-subset.wasm")),
  );
  const hb = instance.exports;
  hb._initialize();

  return async function subsetPdfFont({ codepoints, signal } = {}) {
    signal?.throwIfAborted();
    const normalized = normalizeCodepoints(codepoints);
    // Allow queued cancellation before synchronous HarfBuzz work. Native WASM
    // subsetting itself is synchronous and cannot be interrupted by this signal.
    await setImmediate(undefined, { signal });
    signal?.throwIfAborted();

    let input = 0;
    let pointer = 0;
    let sourceFace = 0;
    let subsetFace = 0;
    let blob = 0;
    try {
      input = hb.hb_subset_input_create_or_fail();
      if (!input) throw new Error("Unable to allocate a font subset input");
      hb.hb_subset_input_set_flags(
        input,
        hb.hb_subset_input_get_flags(input) | HB_SUBSET_FLAGS_RETAIN_GIDS,
      );
      const unicodes = hb.hb_subset_input_unicode_set(input);
      for (const codepoint of normalized) hb.hb_set_add(unicodes, codepoint);
      for (const codepoint of PDF_GSUB_ALIASES)
        hb.hb_set_add(unicodes, codepoint);
      const features = hb.hb_subset_input_set(
        input,
        HB_SUBSET_SETS_LAYOUT_FEATURE_TAG,
      );
      hb.hb_set_clear(features);
      hb.hb_set_invert(features);

      pointer = hb.malloc(source.byteLength);
      if (!pointer) throw new Error("Unable to allocate the configured font");
      new Uint8Array(hb.memory.buffer).set(source, pointer);
      blob = hb.hb_blob_create(
        pointer,
        source.byteLength,
        HB_MEMORY_MODE_WRITABLE,
        0,
        0,
      );
      if (!blob) throw new Error("Unable to load the configured font");
      sourceFace = hb.hb_face_create(blob, 0);
      hb.hb_blob_destroy(blob);
      blob = 0;
      if (!sourceFace || sourceFace === hb.hb_face_get_empty()) {
        throw new Error("Unable to load the configured font face");
      }
      subsetFace = hb.hb_subset_or_fail(sourceFace, input);
      if (!subsetFace) throw new Error("Unable to subset the configured font");
      blob = hb.hb_face_reference_blob(subsetFace);
      const length = hb.hb_blob_get_length(blob);
      if (!length) throw new Error("The generated font subset is empty");
      const offset = hb.hb_blob_get_data(blob, 0);
      // Copy before finally destroys the WASM blob or later calls grow memory.
      return Buffer.from(new Uint8Array(hb.memory.buffer, offset, length));
    } finally {
      if (blob) hb.hb_blob_destroy(blob);
      if (subsetFace) hb.hb_face_destroy(subsetFace);
      if (sourceFace) hb.hb_face_destroy(sourceFace);
      if (pointer) hb.free(pointer);
      if (input) hb.hb_subset_input_destroy(input);
    }
  };
}
