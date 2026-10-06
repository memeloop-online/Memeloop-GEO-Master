// Run in Java 21 source mode against the document-parser shaded PDFBox jar.
// Parses each actual generated PDF, checks extracted glyphs/geometry, renders first/last pages.
import java.awt.image.BufferedImage;
import java.awt.geom.PathIterator;
import java.io.IOException;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import javax.imageio.ImageIO;
import org.apache.pdfbox.Loader;
import org.apache.pdfbox.pdmodel.PDDocument;
import org.apache.pdfbox.pdmodel.PDPage;
import org.apache.pdfbox.pdmodel.font.PDType0Font;
import org.apache.pdfbox.rendering.PDFRenderer;
import org.apache.pdfbox.text.PDFTextStripper;
import org.apache.pdfbox.text.TextPosition;

class VerifyReportPdf {
  static final class PageText extends PDFTextStripper {
    final List<String> issues = new ArrayList<>();
    int hanGlyphs;
    final Map<Integer, Integer> hanOutlines = new HashMap<>();
    int currentPage;

    PageText() throws IOException {
      setSortByPosition(true);
    }

    @Override
    protected void processTextPosition(TextPosition position) {
      float x = position.getXDirAdj();
      float y = position.getYDirAdj();
      // PDFBox uses a top-origin adjusted y; page footer occupies the last 30 pt.
      if (x < 39 || x + position.getWidthDirAdj() > 557
          || y < 30 || y > 821) {
        issues.add("text glyph outside printable page on page " + currentPage);
      }
      if (position.getUnicode().codePoints().anyMatch(
          codepoint -> Character.UnicodeScript.of(codepoint) == Character.UnicodeScript.HAN)
          && position.getFont() instanceof PDType0Font font) {
        for (int code : position.getCharacterCodes()) {
          hanGlyphs++;
          try {
            PathIterator segments = font.getPath(code).getPathIterator(null);
            int outlineHash = 1;
            float[] coordinates = new float[6];
            while (!segments.isDone()) {
              int segmentType = segments.currentSegment(coordinates);
              outlineHash = 31 * outlineHash + segmentType;
              for (int coordinate = 0; coordinate < (segmentType == PathIterator.SEG_CUBICTO ? 6
                  : segmentType == PathIterator.SEG_QUADTO ? 4
                  : segmentType == PathIterator.SEG_CLOSE ? 0 : 2); coordinate++) {
                outlineHash = 31 * outlineHash + Float.floatToIntBits(coordinates[coordinate]);
              }
              segments.next();
            }
            hanOutlines.putIfAbsent(code, outlineHash);
          } catch (IOException error) {
            issues.add("Chinese font path unavailable");
          }
        }
      }
      super.processTextPosition(position);
    }
  }

  static void require(boolean condition, String explanation) {
    if (!condition) throw new IllegalStateException(explanation);
  }

  static void contains(String text, String expected) {
    // PDFBox inserts whitespace at hard visual wraps, including within opaque IDs.
    require(text.replaceAll("\\s+", "").contains(expected.replaceAll("\\s+", "")),
        "missing extracted PDF text: " + expected);
  }

  static void render(PDFRenderer renderer, int page, Path output, String name)
      throws IOException {
    BufferedImage image = renderer.renderImageWithDPI(page, 95);
    require(image.getWidth() > 700 && image.getHeight() > 1000, "rendered page is too small");
    require(ImageIO.write(image, "png", output.resolve(name).toFile()), "PNG writer unavailable");
    image.flush();
  }

  public static void main(String[] args) throws Exception {
    require(args.length == 3, "expected PDF, output directory and fixture kind");
    Path pdfPath = Path.of(args[0]).toAbsolutePath();
    Path output = Path.of(args[1]).toAbsolutePath();
    String kind = args[2];
    require(kind.equals("small") || kind.equals("large"), "unexpected fixture kind");
    String fullText = "";
    int pages;
    try (PDDocument document = Loader.loadPDF(pdfPath.toFile())) {
      pages = document.getNumberOfPages();
      require(pages > (kind.equals("large") ? 2 : 0), "insufficient PDF pages");
      require(document.getDocumentInformation().getTitle().contains("修订 3"),
          "snapshot revision missing from PDF metadata");
      for (int index = 0; index < pages; index++) {
        PDPage page = document.getPage(index);
        require(Math.abs(page.getMediaBox().getWidth() - 595.28) < 0.2
            && Math.abs(page.getMediaBox().getHeight() - 841.89) < 0.2,
            "PDF page is not A4");
        PageText extractor = new PageText();
        extractor.currentPage = index + 1;
        extractor.setStartPage(index + 1);
        extractor.setEndPage(index + 1);
        String text = extractor.getText(document);
        require(extractor.issues.isEmpty(),
            extractor.issues.isEmpty() ? "" : extractor.issues.getFirst());
        require(extractor.hanGlyphs > 0, "page has no embedded Chinese glyphs");
        // Simple genuine glyphs can have few path segments; compare distinct
        // outlines instead of rejecting such glyphs individually.
        require(extractor.hanOutlines.values().stream().distinct().count() >= 6,
            "Chinese glyph outlines collapse to fallback boxes on page " + (index + 1));
        contains(text, "不可变周报");
        contains(text, (index + 1) + " / " + pages);
        require(!text.contains("\uFFFD") && !text.contains("[U+"), "replacement or fallback glyph");
        require(text.codePoints().noneMatch(codepoint -> codepoint >= 0xE000
            && codepoint <= 0xF8FF), "synthetic text changed into private-use font codepoints");
        fullText += text;
      }
      PDFRenderer renderer = new PDFRenderer(document);
      render(renderer, 0, output, "report-" + kind + "-first.png");
      render(renderer, pages - 1, output, "report-" + kind + "-last.png");
    }
    contains(fullText, "中文结论");
    contains(fullText, "冻结计划分母：未知");
    contains(fullText, "冻结计划分母：0");
    contains(fullText, "冻结状态 unknown：1");
    contains(fullText, "冻结状态 missing：1");
    contains(fullText, "冻结状态 not_mentioned：1");
    contains(fullText, "report-" + kind + "-immutable");
    contains(fullText, "report-prior-immutable");
    contains(fullText, "FROZEN_INPUT_HASH_3579");
    contains(fullText, "修订 3");
    contains(fullText, "evidence-999");
    contains(fullText, "END_OF_LAST_EVIDENCE_4960");
    require(!fullText.contains("INJECTED_SECRET_SHOULD_NEVER_APPEAR_5932"),
        "unexpected private field leaked into PDF");
    System.out.println("Extracted and rendered " + kind + " PDF pages: " + pages);
  }
}
