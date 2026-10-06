package dev.geo.parser;

import static org.junit.jupiter.api.Assertions.*;

import java.awt.Color;
import java.awt.image.BufferedImage;
import java.io.ByteArrayOutputStream;
import java.net.InetSocketAddress;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.security.MessageDigest;
import java.util.HexFormat;
import java.util.concurrent.atomic.AtomicLong;
import org.apache.pdfbox.Loader;
import org.apache.pdfbox.cos.COSName;
import org.apache.pdfbox.pdmodel.PDDocument;
import org.apache.pdfbox.pdmodel.PDPage;
import org.apache.pdfbox.pdmodel.PDPageContentStream;
import org.apache.pdfbox.pdmodel.common.PDRectangle;
import org.apache.pdfbox.pdmodel.font.PDType1Font;
import org.apache.pdfbox.pdmodel.font.Standard14Fonts;
import org.apache.pdfbox.pdmodel.graphics.image.LosslessFactory;
import org.apache.pdfbox.pdmodel.graphics.image.PDImageXObject;
import org.apache.pdfbox.pdmodel.encryption.AccessPermission;
import org.apache.pdfbox.pdmodel.encryption.StandardProtectionPolicy;
import org.junit.jupiter.api.Test;

class PdfParserTest {
    private static byte[] twoPages() throws Exception {
        try (PDDocument document = new PDDocument()) {
            addTextPage(document, "Résumé café");
            addTextPage(document, "Zürich €");
            return save(document);
        }
    }

    private static void addTextPage(PDDocument document, String text) throws Exception {
        PDPage page = new PDPage(PDRectangle.LETTER);
        document.addPage(page);
        try (PDPageContentStream content = new PDPageContentStream(document, page)) {
            content.beginText();
            content.setFont(new PDType1Font(Standard14Fonts.FontName.HELVETICA), 14);
            content.newLineAtOffset(45, 700);
            content.showText(text);
            content.endText();
        }
    }

    private static byte[] save(PDDocument document) throws Exception {
        ByteArrayOutputStream output = new ByteArrayOutputStream();
        document.save(output);
        return output.toByteArray();
    }

    @Test
    void extractsEachOriginalPageWithUnicodeAndRawInputHash() throws Exception {
        byte[] pdf = twoPages();
        String sha = HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(pdf));
        var inspected = PdfDocumentParser.inspect(pdf);
        assertEquals(2, inspected.pageCount());
        assertEquals(sha, inspected.inputSha256());
        var first = PdfDocumentParser.parsePage(pdf, 1);
        var second = PdfDocumentParser.parsePage(pdf, 2);
        assertEquals(sha, first.inputSha256());
        assertEquals(sha, second.inputSha256());
        assertEquals(PdfDocumentParser.VERSION, "tika-3.2.3_pdfbox-3.0.5_text-v1");
        assertTrue(first.text().contains("Résumé café"), first.text());
        assertFalse(first.text().contains("Zürich"));
        assertTrue(second.text().contains("Zürich €"), second.text());
        assertFalse(second.text().contains("Résumé"));
        assertNull(first.reason());
        assertEquals(2, second.page());
    }

    @Test
    void pageRangeMalformedAndEncryptedAreStaticFailures() throws Exception {
        byte[] pdf = twoPages();
        assertEquals("page_out_of_range", assertThrows(PdfDocumentParser.ParseFailure.class,
                () -> PdfDocumentParser.parsePage(pdf, 0)).code());
        assertEquals("page_out_of_range", assertThrows(PdfDocumentParser.ParseFailure.class,
                () -> PdfDocumentParser.parsePage(pdf, 3)).code());
        assertEquals("invalid_pdf", assertThrows(PdfDocumentParser.ParseFailure.class,
                () -> PdfDocumentParser.inspect(new byte[] {1, 2, 3})).code());
        try (PDDocument encrypted = new PDDocument()) {
            addTextPage(encrypted, "Encrypted");
            encrypted.protect(new StandardProtectionPolicy("owner", "user", new AccessPermission()));
            byte[] protectedBytes = save(encrypted);
            assertEquals("encrypted_pdf", assertThrows(PdfDocumentParser.ParseFailure.class,
                    () -> PdfDocumentParser.inspect(protectedBytes)).code());
        }
    }

    @Test
    void imageOnlyRequiresOcrAndBlankPageIsExplicitlyEmpty() throws Exception {
        try (PDDocument document = new PDDocument()) {
            PDPage imagePage = new PDPage(PDRectangle.LETTER);
            document.addPage(imagePage);
            BufferedImage image = new BufferedImage(24, 24, BufferedImage.TYPE_INT_RGB);
            var graphics = image.createGraphics();
            graphics.setColor(Color.BLACK);
            graphics.fillRect(0, 0, 24, 24);
            graphics.dispose();
            PDImageXObject xobject = LosslessFactory.createFromImage(document, image);
            try (PDPageContentStream content = new PDPageContentStream(document, imagePage)) {
                content.drawImage(xobject, 10, 10);
            }
            document.addPage(new PDPage(PDRectangle.LETTER));
            byte[] pdf = save(document);
            var first = PdfDocumentParser.parsePage(pdf, 1);
            assertEquals("", first.text());
            assertEquals("ocr_required", first.reason());
            var second = PdfDocumentParser.parsePage(pdf, 2);
            assertEquals("", second.text());
            assertEquals("empty_text", second.reason());
        }
    }

    @Test
    void importsInheritedPageTreeResourcesWithoutLosingTextOrOriginalPageNumber() throws Exception {
        try (PDDocument document = new PDDocument()) {
            addTextPage(document, "Before inherited");
            addTextPage(document, "Inherited café");
            PDPage page = document.getPage(1);
            page.setMediaBox(new PDRectangle(777, 999));
            page.setCropBox(new PDRectangle(760, 960));
            var resources = page.getCOSObject().getDictionaryObject(COSName.RESOURCES);
            var mediaBox = page.getCOSObject().getDictionaryObject(COSName.MEDIA_BOX);
            var cropBox = page.getCOSObject().getDictionaryObject(COSName.CROP_BOX);
            assertNotNull(resources);
            // The second page has no local resources, page box or rotation.
            document.getDocumentCatalog().getPages().getCOSObject()
                    .setItem(COSName.RESOURCES, resources);
            document.getDocumentCatalog().getPages().getCOSObject()
                    .setItem(COSName.MEDIA_BOX, mediaBox);
            document.getDocumentCatalog().getPages().getCOSObject()
                    .setItem(COSName.CROP_BOX, cropBox);
            page.getCOSObject().removeItem(COSName.RESOURCES);
            page.getCOSObject().removeItem(COSName.MEDIA_BOX);
            page.getCOSObject().removeItem(COSName.CROP_BOX);
            document.getDocumentCatalog().getPages().getCOSObject()
                    .setInt(COSName.ROTATE, 90);
            assertNull(page.getCOSObject().getDictionaryObject(COSName.RESOURCES));
            byte[] pdf = save(document);
            byte[] importedBytes;
            try (PDDocument reopened = Loader.loadPDF(pdf);
                    PDDocument imported = new PDDocument()) {
                PDPage child = PdfDocumentParser.importPreservingInheritedAttributes(
                        imported, reopened.getPage(1));
                assertNotNull(child.getResources(), "import must preserve inherited font resources");
                assertEquals(90, child.getRotation(), "import must preserve inherited rotation");
                assertEquals(999, child.getMediaBox().getHeight());
                assertEquals(960, child.getCropBox().getHeight());
                importedBytes = save(imported);
            }
            assertTrue(PdfDocumentParser.parsePage(importedBytes, 1).text()
                    .contains("Inherited café"), "isolated page must not depend on source document");
            var result = PdfDocumentParser.parsePage(pdf, 2);
            assertEquals(2, result.page());
            assertTrue(result.text().contains("Inherited café"), result.text());
            assertFalse(result.text().contains("Before inherited"));
        }
    }

    @Test
    void httpResponsesUseStableContractWithoutParserExceptions() throws Exception {
        assertEquals("{\"error\":{\"code\":\"parser_busy\",\"retryable\":true}}",
                PdfServer.error("parser_busy"));
        assertEquals("{\"error\":{\"code\":\"invalid_request\",\"retryable\":false}}",
                PdfServer.error("invalid_request"));
        assertEquals("{\"error\":{\"code\":\"parser_timeout\",\"retryable\":true}}",
                PdfServer.error("parser_timeout"));
        PdfServer server = new PdfServer(new InetSocketAddress("127.0.0.1", 0), 2);
        server.start();
        try {
            byte[] pdf = twoPages();
            String base = "http://127.0.0.1:" + server.port();
            HttpClient client = HttpClient.newHttpClient();
            var health = client.send(HttpRequest.newBuilder(URI.create(base + "/health"))
                            .GET().build(), HttpResponse.BodyHandlers.ofString());
            assertEquals(200, health.statusCode());
            assertTrue(health.body().contains("\"parser_version\":\""
                    + PdfDocumentParser.VERSION + "\""));
            assertTrue(health.body().contains("\"capacity\":2"));
            var inspected = send(client, base + "/v1/pdf/inspect", pdf);
            assertEquals(200, inspected.statusCode());
            assertTrue(inspected.body().contains("\"schema_version\":\"geo.pdf.parse.v1\""));
            assertTrue(inspected.body().contains("\"page_count\":2"));
            assertTrue(inspected.body().contains(PdfDocumentParser.inspect(pdf).inputSha256()));
            var parsed = send(client, base + "/v1/pdf/pages/2/parse", pdf);
            assertEquals(200, parsed.statusCode());
            assertTrue(parsed.body().contains("Zürich €"));
            assertTrue(parsed.body().contains(PdfDocumentParser.inspect(pdf).inputSha256()));
            assertEquals(404, send(client, base + "/v1/pdf/pages/3/parse", pdf).statusCode());
            assertTrue(send(client, base + "/v1/pdf/pages/3/parse", pdf).body()
                    .contains("\"code\":\"page_out_of_range\""));
        } finally {
            server.stop();
        }
    }

    @Test
    void timedOutChildIsDeadAndReapedBeforeNextRealPdfRequest() throws Exception {
        AtomicLong pid = new AtomicLong();
        var timedOut = WorkerProcess.executeProbe("hang", 150, pid);
        assertEquals("parser_timeout", timedOut.error());
        assertTrue(pid.get() > 0);
        assertFalse(ProcessHandle.of(pid.get()).map(ProcessHandle::isAlive).orElse(false));
        assertEquals(2, PdfDocumentParser.inspect(twoPages()).pageCount());
        PdfServer server = new PdfServer(new InetSocketAddress("127.0.0.1", 0), 1);
        server.start();
        try {
            var fresh = send(HttpClient.newHttpClient(),
                    "http://127.0.0.1:" + server.port() + "/v1/pdf/inspect", twoPages());
            assertEquals(200, fresh.statusCode(), fresh.body());
            assertTrue(fresh.body().contains("\"page_count\":2"));
        } finally {
            server.stop();
        }
    }

    @Test
    void oversizedOrFailedChildNeverReturnsRawOutput() {
        assertEquals("{}", WorkerProcess.executeProbe("env", 10_000,
                new AtomicLong()).json(), "child must not inherit service credentials");
        assertEquals("output_too_large",
                WorkerProcess.executeProbe("oversize", 10_000, new AtomicLong()).error());
        assertEquals("parser_failed",
                WorkerProcess.executeProbe("exit", 10_000, new AtomicLong()).error());
    }

    private static HttpResponse<String> send(HttpClient client, String url, byte[] body)
            throws Exception {
        return client.send(HttpRequest.newBuilder(URI.create(url))
                        .header("Content-Type", "application/pdf")
                        .POST(HttpRequest.BodyPublishers.ofByteArray(body))
                        .build(),
                HttpResponse.BodyHandlers.ofString());
    }
}
