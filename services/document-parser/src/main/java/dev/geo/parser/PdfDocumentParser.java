package dev.geo.parser;

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.Collections;
import java.util.IdentityHashMap;
import java.util.Set;
import org.apache.pdfbox.cos.COSDictionary;
import org.apache.pdfbox.cos.COSName;
import org.apache.pdfbox.Loader;
import org.apache.pdfbox.contentstream.PDContentStream;
import org.apache.pdfbox.contentstream.operator.Operator;
import org.apache.pdfbox.contentstream.operator.OperatorName;
import org.apache.pdfbox.multipdf.PDFCloneUtility;
import org.apache.pdfbox.pdfparser.PDFStreamParser;
import org.apache.pdfbox.pdmodel.PDDocument;
import org.apache.pdfbox.pdmodel.PDPage;
import org.apache.pdfbox.pdmodel.PDResources;
import org.apache.pdfbox.pdmodel.graphics.PDXObject;
import org.apache.pdfbox.pdmodel.graphics.form.PDFormXObject;
import org.apache.pdfbox.pdmodel.graphics.image.PDImageXObject;
import org.apache.pdfbox.pdmodel.encryption.InvalidPasswordException;
import org.apache.tika.exception.TikaException;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.parser.ParseContext;
import org.apache.tika.parser.pdf.PDFParser;
import org.apache.tika.parser.pdf.PDFParserConfig;
import org.apache.tika.extractor.EmbeddedDocumentExtractor;
import org.apache.tika.sax.BodyContentHandler;
import org.xml.sax.ContentHandler;
import org.xml.sax.SAXException;

/** Untrusted PDF bytes in; no network, attachment extraction, OCR or persistent output. */
public final class PdfDocumentParser {
    public static final String SCHEMA = "geo.pdf.parse.v1";
    public static final String VERSION = "tika-3.2.3_pdfbox-3.0.5_text-v1";
    public static final int MAX_INPUT = 100 * 1024 * 1024;
    private static final int MAX_PAGES = 10_000;
    private static final int MAX_PAGE_PDF = 8 * 1024 * 1024;
    private static final int MAX_TEXT_CHARS = 4_000_000;
    private static final int MAX_TEXT_BYTES = 4 * 1024 * 1024;

    public record Inspection(String inputSha256, int pageCount) {}
    public record PageText(String inputSha256, int page, String text, String reason) {}

    public static final class ParseFailure extends Exception {
        private final String code;
        public ParseFailure(String code) {
            super(code);
            this.code = code;
        }
        public String code() { return code; }
    }

    private PdfDocumentParser() {}

    public static Inspection inspect(byte[] bytes) throws ParseFailure {
        try (PDDocument document = load(bytes)) {
            return new Inspection(sha256(bytes), countPages(document));
        } catch (IOException | RuntimeException ex) {
            throw new ParseFailure("invalid_pdf");
        }
    }

    public static PageText parsePage(byte[] bytes, int page) throws ParseFailure {
        try (PDDocument document = load(bytes)) {
            int count = countPages(document);
            if (page < 1 || page > count) throw new ParseFailure("page_out_of_range");
            PDPage original = document.getPage(page - 1);
            boolean hasImages = hasRasterImages(original.getResources(),
                    Collections.newSetFromMap(new IdentityHashMap<>()), 0);
            try (PDDocument singlePage = new PDDocument()) {
                importPreservingInheritedAttributes(singlePage, original);
                LimitedBytes saved = new LimitedBytes(MAX_PAGE_PDF);
                singlePage.save(saved);
                PDFParser tika = new PDFParser();
                PDFParserConfig config = new PDFParserConfig();
                config.setOcrStrategy(PDFParserConfig.OCR_STRATEGY.NO_OCR);
                config.setExtractInlineImages(false);
                ParseContext context = new ParseContext();
                context.set(PDFParserConfig.class, config);
                // PDF attachments are not input pages. Never parse their bytes.
                context.set(EmbeddedDocumentExtractor.class, new EmbeddedDocumentExtractor() {
                    @Override public boolean shouldParseEmbedded(Metadata metadata) { return false; }
                    @Override public void parseEmbedded(
                            java.io.InputStream stream, ContentHandler handler,
                            Metadata metadata, boolean outputHtml) {}
                });
                BodyContentHandler content = new BodyContentHandler(MAX_TEXT_CHARS);
                try {
                    tika.parse(new ByteArrayInputStream(saved.toByteArray()),
                            content, new Metadata(), context);
                } catch (SAXException | TikaException ex) {
                    if (isWriteLimit(ex)) throw new ParseFailure("output_too_large");
                    throw new ParseFailure("invalid_pdf");
                }
                String text = content.toString().trim();
                if (text.getBytes(StandardCharsets.UTF_8).length > MAX_TEXT_BYTES)
                    throw new ParseFailure("output_too_large");
                String reason = text.isEmpty()
                        ? (hasImages || hasInlineImages(original) ? "ocr_required" : "empty_text")
                        : null;
                return new PageText(sha256(bytes), page, text, reason);
            }
        } catch (LimitExceeded ex) {
            throw new ParseFailure("output_too_large");
        } catch (IOException ex) {
            throw new ParseFailure("invalid_pdf");
        } catch (RuntimeException ex) {
            // Malformed PDFs can also trigger unchecked parser exceptions.
            throw new ParseFailure("invalid_pdf");
        }
    }

    private static PDDocument load(byte[] bytes) throws ParseFailure {
        if (bytes.length == 0) throw new ParseFailure("invalid_pdf");
        if (bytes.length > MAX_INPUT) throw new ParseFailure("input_too_large");
        try {
            PDDocument document = Loader.loadPDF(bytes);
            if (document.isEncrypted()) {
                document.close();
                throw new ParseFailure("encrypted_pdf");
            }
            return document;
        } catch (InvalidPasswordException ex) {
            throw new ParseFailure("encrypted_pdf");
        } catch (IOException | RuntimeException ex) {
            throw new ParseFailure("invalid_pdf");
        }
    }

    private static int countPages(PDDocument document) throws ParseFailure {
        int pages = document.getNumberOfPages();
        if (pages <= 0) throw new ParseFailure("invalid_pdf");
        if (pages > MAX_PAGES) throw new ParseFailure("page_limit_exceeded");
        return pages;
    }

    private static boolean hasRasterImages(PDResources resources, Set<PDResources> seen, int depth)
            throws IOException {
        if (resources == null || depth > 8 || !seen.add(resources)) return false;
        for (var name : resources.getXObjectNames()) {
            PDXObject object = resources.getXObject(name);
            if (object instanceof PDImageXObject) return true;
            if (object instanceof PDFormXObject form
                    && hasRasterImages(form.getResources(), seen, depth + 1)) return true;
        }
        return false;
    }

    // Inline BI/ID/EI images are in page content, not /Resources /XObject.
    // Scan operators only when no text or XObject was found; never decode OCR pixels.
    private static boolean hasInlineImages(PDPage page) throws IOException {
        if (!page.hasContents()) return false;
        return hasInlineImages(page, Collections.newSetFromMap(new IdentityHashMap<>()), 0);
    }

    private static boolean hasInlineImages(PDContentStream content,
            Set<COSDictionary> seenForms, int depth) throws IOException {
        if (depth > 8) return false;
        PDFStreamParser parser = new PDFStreamParser(content);
        Object token;
        while ((token = parser.parseNextToken()) != null) {
            if (token instanceof Operator operator
                    && OperatorName.BEGIN_INLINE_IMAGE.equals(operator.getName())) return true;
        }
        PDResources resources = content.getResources();
        if (resources != null) {
            for (var name : resources.getXObjectNames()) {
                PDXObject object = resources.getXObject(name);
                if (object instanceof PDFormXObject form
                        && seenForms.add(form.getCOSObject())
                        && hasInlineImages(form, seenForms, depth + 1)) return true;
            }
        }
        return false;
    }

    /**
     * PDFBox importPage does not copy inherited /Resources, /MediaBox, /CropBox or /Rotate.
     * Materialize them on the child page before saving it as an independent one-page PDF.
     */
    static PDPage importPreservingInheritedAttributes(PDDocument destination, PDPage original)
            throws IOException {
        PDPage imported = destination.importPage(original);
        if (imported.getCOSObject().getDictionaryObject(COSName.RESOURCES) == null
                && original.getResources() != null) {
            var clone = new PageResourcesClone(destination);
            imported.setResources(new PDResources((COSDictionary) clone.cloneForNewDocument(
                    original.getResources().getCOSObject())));
        }
        imported.setMediaBox(original.getMediaBox());
        imported.setCropBox(original.getCropBox());
        imported.setRotation(original.getRotation());
        return imported;
    }

    private static boolean isWriteLimit(Throwable ex) {
        for (Throwable current = ex; current != null; current = current.getCause()) {
            if (current.getClass().getSimpleName().equals("WriteLimitReachedException")) return true;
        }
        return false;
    }

    private static final class PageResourcesClone extends PDFCloneUtility {
        PageResourcesClone(PDDocument destination) { super(destination); }
    }

    private static String sha256(byte[] bytes) {
        try {
            byte[] digest = MessageDigest.getInstance("SHA-256").digest(bytes);
            return java.util.HexFormat.of().formatHex(digest);
        } catch (NoSuchAlgorithmException impossible) {
            throw new AssertionError(impossible);
        }
    }

    private static final class LimitExceeded extends RuntimeException {}

    private static final class LimitedBytes extends ByteArrayOutputStream {
        private final int limit;
        LimitedBytes(int limit) { this.limit = limit; }
        @Override public synchronized void write(int value) {
            if (count >= limit) throw new LimitExceeded();
            super.write(value);
        }
        @Override public synchronized void write(byte[] bytes, int offset, int length) {
            if (length > limit - count) throw new LimitExceeded();
            super.write(bytes, offset, length);
        }
    }
}
