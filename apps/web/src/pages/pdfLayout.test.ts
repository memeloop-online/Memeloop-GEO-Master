import { afterEach, describe, expect, it, vi } from "vitest";
import { PDFDocument, PDFPage, type PDFFont } from "pdf-lib";
import fontkit from "@pdf-lib/fontkit";
import { fullFontBytes, subsetFontResponse } from "./pdfFontSubsetTestHelper";
import {
  PdfLayout,
  PdfTextPlan,
  createPdfDocument,
  wrapPdfText,
  yieldToBrowser,
} from "./pdfLayout";

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("shared PDF layout", () => {
  it("collects every operation, localized footer digit and printable fallback while excluding lone surrogates", () => {
    const number = new Intl.NumberFormat("ar-EG");
    const footer = (page: number, total: number) =>
      `revision 37 · ${number.format(page)} / ${number.format(total)}`;
    const plan = new PdfTextPlan(footer);
    plan.heading("Heading 标题");
    plan.text("Early text");
    plan.text(
      "Late appendix 末 \u{1F9EA}\uD800\uDC00\uD800 e\u0301 \u1100\u1161",
    );
    const codepoints = plan.codepoints();
    const expected = `Heading 标题Early textLate appendix 末 \u{1F9EA}\u{10000} e\u0301 \u1100\u1161 é 가[U+0123456789ABCDEF]中\u25CC${footer(1234567890, 1234567890)}`;
    expect(
      [...expected].every((character) =>
        codepoints.includes(character.codePointAt(0)!),
      ),
    ).toBe(true);
    expect(codepoints).toEqual([...new Set(codepoints)].sort((a, b) => a - b));
    expect(
      codepoints.some(
        (codepoint) => codepoint >= 0xd800 && codepoint <= 0xdfff,
      ),
    ).toBe(false);
  });

  it.each([
    "e\u0301",
    "\u1100\u1161",
    "\u1100\u1161\u11A8",
    "\u302E",
    "\u302F",
    "\u1100\u302E",
  ])(
    "preserves full-font shaping of composed text and synthesized marks for %s",
    async (value) => {
      const plan = new PdfTextPlan(() => "footer");
      plan.text(value);
      const response = await subsetFontResponse("/api/v1/pdf/font-subset", {
        body: JSON.stringify({ codepoints: plan.codepoints() }),
      });
      const subset = fontkit.create(
        new Uint8Array(await response.arrayBuffer()),
      );
      const original = fontkit.create(fullFontBytes);
      const shape = (font: typeof original) => {
        const run = font.layout(value, { locl: false });
        return {
          glyphs: run.glyphs.map((glyph) => ({
            id: glyph.id,
            codePoints: glyph.codePoints,
            advanceWidth: glyph.advanceWidth,
          })),
          positions: run.positions,
        };
      };
      expect(shape(subset)).toEqual(shape(original));
    },
  );

  it("keeps unsupported combining marks in the same printable fallback", async () => {
    const value = "a\u030A";
    const plan = new PdfTextPlan(() => "footer");
    plan.text(value);
    const response = await subsetFontResponse("/api/v1/pdf/font-subset", {
      body: JSON.stringify({ codepoints: plan.codepoints() }),
    });
    const errors = {
      missing: "missing",
      invalid: "invalid",
      download: "download",
      incomplete: "incomplete",
    };
    const drawn = vi.spyOn(PDFPage.prototype, "drawText");
    const baseline = await createPdfDocument(fullFontBytes, errors);
    new PdfLayout(baseline.document, baseline.font, () => "footer").text(value);
    const originalText = drawn.mock.calls.map(([text]) => text);
    drawn.mockClear();
    const subset = await createPdfDocument(
      new Uint8Array(await response.arrayBuffer()),
      errors,
    );
    new PdfLayout(subset.document, subset.font, () => "footer").text(value);
    expect(drawn.mock.calls.map(([text]) => text)).toEqual(originalText);
    expect(originalText).toEqual(["a[U+030A]"]);
  });

  it("copies options and replays the same operation order through the existing layout", async () => {
    const plan = new PdfTextPlan(() => "footer");
    const options = { indent: 10, size: 9 };
    plan.text("first", options);
    plan.heading("second");
    plan.text("third e\u0301 \u1100\u1161");
    options.indent = 200;
    const operations: unknown[] = [];
    vi.spyOn(PdfLayout.prototype, "text").mockImplementation((...args) => {
      operations.push(["text", ...args]);
    });
    vi.spyOn(PdfLayout.prototype, "heading").mockImplementation((...args) => {
      operations.push(["heading", ...args]);
    });
    const finish = vi
      .spyOn(PdfLayout.prototype, "finish")
      .mockImplementation(() => {});
    await plan.replay(await PDFDocument.create(), {
      getCharacterSet: () => [],
    } as unknown as PDFFont);
    expect(operations).toEqual([
      ["text", "first", { indent: 10, size: 9 }],
      ["heading", "second"],
      ["text", "third e\u0301 \u1100\u1161", {}],
    ]);
    expect(finish).toHaveBeenCalledOnce();
  });
  it("prefers whole Latin words without losing whitespace or changing the legacy default", () => {
    const width = (value: string) => [...value].length;
    expect(wrapPdfText("alpha beta gamma", 10, width)).toEqual([
      "alpha beta",
      " gamma",
    ]);
    expect(wrapPdfText("alpha beta gamma", 10, width, true)).toEqual([
      "alpha ",
      "beta gamma",
    ]);
    const value = `ASCII words 中文证据 ${"opaque-key".repeat(20)}\n  preserved spaces`;
    const lines = wrapPdfText(value, 13, width, true);
    expect(lines.join("")).toBe(value.replace(/\n/gu, ""));
    expect(lines.every((line) => width(line) <= 13)).toBe(true);
  });

  it("never overflows when a wide glyph follows a long word", () => {
    const width = (value: string) =>
      [...value].reduce(
        (total, character) => total + (character === "中" ? 3 : 1),
        0,
      );
    const value = "a opaque中final";
    const lines = wrapPdfText(value, 8, width, true);
    expect(lines.join("")).toBe(value);
    expect(lines.every((line) => width(line) <= 8)).toBe(true);
  });

  it.each([0, 20])(
    "honors cancellation after yielding at item %i",
    async (index) => {
      vi.useFakeTimers();
      const controller = new AbortController();
      const pending = yieldToBrowser(index, controller.signal);
      const rejected = expect(pending).rejects.toMatchObject({
        name: "AbortError",
      });
      controller.abort();
      await vi.runAllTimersAsync();
      await rejected;
    },
  );
});
