import fontkit from "@pdf-lib/fontkit";
import { PDFDocument, rgb, type PDFFont, type PDFPage } from "pdf-lib";

const PAGE_WIDTH = 595.28; // A4 in PDF points
const PAGE_HEIGHT = 841.89;
const MARGIN = 46;
const CONTENT_WIDTH = PAGE_WIDTH - MARGIN * 2;
const FOOTER_Y = 27;
const CONTENT_BOTTOM = 53;
const INK = rgb(0.13, 0.17, 0.23);
const MUTED = rgb(0.35, 0.4, 0.46);

/** Measure and split by Unicode code point, so even an unbroken opaque key fits. */
export function wrapPdfText(
  value: string,
  maxWidth: number,
  widthOf: (value: string) => number,
  preferWordBoundaries = false,
): string[] {
  if (maxWidth <= 0) throw new Error("PDF text width must be positive");
  const lines: string[] = [];
  for (const paragraph of value.split(/\r\n|\r|\n/u)) {
    let line = "";
    for (const character of paragraph) {
      while (line && widthOf(line + character) > maxWidth) {
        const boundary = preferWordBoundaries ? line.lastIndexOf(" ") : -1;
        if (boundary > 0) {
          lines.push(line.slice(0, boundary + 1));
          line = line.slice(boundary + 1);
        } else {
          lines.push(line);
          line = "";
        }
      }
      // A glyph wider than the usable page width cannot be clipped silently.
      if (widthOf(character) > maxWidth) {
        throw new Error("PDF 字体中有字符无法适配页面，请更换字体后重试。");
      }
      line += character;
    }
    lines.push(line);
  }
  return lines;
}

function printable(value: string, supported: Set<number>): string {
  return [...value]
    .map((character) => {
      const code = character.codePointAt(0)!;
      if (character === "\n" || character === "\r") return character;
      if (code < 32 || code === 127 || !supported.has(code)) {
        return `[U+${code.toString(16).toUpperCase().padStart(4, "0")}]`;
      }
      return character;
    })
    .join("");
}

export class PdfLayout {
  private page!: PDFPage;
  private y = 0;
  private readonly pages: PDFPage[] = [];
  private readonly supported: Set<number>;

  constructor(
    private readonly document: PDFDocument,
    private readonly font: PDFFont,
    private readonly footer: (page: number, total: number) => string,
    private readonly signal?: AbortSignal,
    private readonly preferWordBoundaries = false,
  ) {
    this.supported = new Set(font.getCharacterSet());
    this.newPage();
  }

  private newPage(): void {
    this.page = this.document.addPage([PAGE_WIDTH, PAGE_HEIGHT]);
    this.pages.push(this.page);
    this.y = PAGE_HEIGHT - MARGIN;
  }

  private ensureSpace(height: number): void {
    if (this.y - height < CONTENT_BOTTOM) this.newPage();
  }

  private textWidth(value: string, size: number): number {
    return this.font.widthOfTextAtSize(value, size);
  }

  private drawLine(
    line: string,
    x: number,
    y: number,
    size: number,
    muted: boolean,
    page = this.page,
  ): void {
    page.drawText(line, {
      x,
      y,
      size,
      font: this.font,
      color: muted ? MUTED : INK,
    });
  }

  text(
    value: string,
    options: {
      size?: number;
      indent?: number;
      gap?: number;
      muted?: boolean;
    } = {},
  ): void {
    this.signal?.throwIfAborted();
    const size = options.size ?? 9;
    const indent = options.indent ?? 0;
    const lineHeight = size * 1.55;
    const width = CONTENT_WIDTH - indent;
    const clean = printable(value, this.supported);
    const lines = wrapPdfText(
      clean,
      width,
      (candidate) => this.textWidth(candidate, size),
      this.preferWordBoundaries,
    );
    for (const line of lines) {
      this.ensureSpace(lineHeight);
      this.drawLine(
        line,
        MARGIN + indent,
        this.y - size,
        size,
        !!options.muted,
      );
      this.y -= lineHeight;
    }
    this.y -= options.gap ?? 3;
  }

  heading(title: string): void {
    const headingLines = wrapPdfText(
      printable(title, this.supported),
      CONTENT_WIDTH,
      (line) => this.textWidth(line, 13),
      this.preferWordBoundaries,
    ).length;
    this.ensureSpace(12 + headingLines * (13 * 1.55) + 8 + 9 * 1.55);
    this.y -= 12;
    this.text(title, { size: 13, gap: 8 });
  }

  finish(): void {
    for (const [index, page] of this.pages.entries()) {
      const footer = this.footer(index + 1, this.pages.length);
      const lines = wrapPdfText(
        printable(footer, this.supported),
        CONTENT_WIDTH,
        (line) => this.textWidth(line, 8),
      );
      for (const [lineIndex, line] of lines.entries()) {
        this.drawLine(
          line,
          MARGIN,
          FOOTER_Y + (lines.length - lineIndex - 1) * 9,
          8,
          true,
          page,
        );
      }
    }
  }
}

export async function yieldToBrowser(
  index: number,
  signal?: AbortSignal,
): Promise<void> {
  signal?.throwIfAborted();
  if (index > 0 && index % 20 === 0) {
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    signal?.throwIfAborted();
  }
}

export type PdfFontErrors = {
  missing: string;
  invalid: string;
  download: string;
  incomplete: string;
};

/** Both report families embed the same full CJK font and preserve ASCII IDs. */
export async function createPdfDocument(
  fontBytes: Uint8Array,
  errors: PdfFontErrors,
) {
  if (!fontBytes.length) throw new Error(errors.missing);
  const document = await PDFDocument.create();
  document.registerFontkit(fontkit);
  try {
    // CFF subsetting loses Chinese outlines in PDFBox; localized digits lose
    // searchable ASCII IDs in pdf-lib's ToUnicode table.
    const font = await document.embedFont(fontBytes, {
      subset: false,
      features: { locl: false },
    });
    if (!font.getCharacterSet().includes("中".codePointAt(0)!)) {
      throw new Error("Chinese glyph missing");
    }
    return { document, font };
  } catch {
    throw new Error(errors.invalid);
  }
}

export async function loadPdfFont(
  errors: PdfFontErrors,
  signal?: AbortSignal,
): Promise<Uint8Array> {
  signal?.throwIfAborted();
  let response: Response;
  try {
    response = await fetch("/fonts/NotoSansCJKsc-Regular.otf", {
      credentials: "same-origin",
      signal,
    });
    if (!response.ok) throw new Error("Font unavailable");
  } catch (error) {
    signal?.throwIfAborted();
    if (error instanceof DOMException && error.name === "AbortError")
      throw error;
    throw new Error(errors.download);
  }
  signal?.throwIfAborted();
  try {
    const bytes = new Uint8Array(await response.arrayBuffer());
    signal?.throwIfAborted();
    return bytes;
  } catch {
    signal?.throwIfAborted();
    throw new Error(errors.incomplete);
  }
}

export function downloadPdfBytes(
  bytes: Uint8Array,
  filename: string,
  signal?: AbortSignal,
): void {
  signal?.throwIfAborted();
  const blob = new Blob([new Uint8Array(bytes)], { type: "application/pdf" });
  signal?.throwIfAborted();
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  try {
    link.href = url;
    link.download = filename;
    link.hidden = true;
    document.body.append(link);
    signal?.throwIfAborted();
    link.click();
  } finally {
    link.remove();
    URL.revokeObjectURL(url);
  }
}
