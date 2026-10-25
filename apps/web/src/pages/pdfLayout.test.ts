import { afterEach, describe, expect, it, vi } from "vitest";
import { wrapPdfText, yieldToBrowser } from "./pdfLayout";

afterEach(() => vi.useRealTimers());

describe("shared PDF layout", () => {
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

  it("honors cancellation after yielding to browser input", async () => {
    vi.useFakeTimers();
    const controller = new AbortController();
    const pending = yieldToBrowser(20, controller.signal);
    const rejected = expect(pending).rejects.toMatchObject({
      name: "AbortError",
    });
    controller.abort();
    await vi.runAllTimersAsync();
    await rejected;
  });
});
