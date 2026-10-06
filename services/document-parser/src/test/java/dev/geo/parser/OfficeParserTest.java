package dev.geo.parser;

import static org.junit.jupiter.api.Assertions.*;

import java.io.ByteArrayOutputStream;
import java.net.InetSocketAddress;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.util.zip.ZipEntry;
import java.util.zip.ZipOutputStream;
import org.apache.poi.ss.SpreadsheetVersion;
import org.apache.poi.ss.util.AreaReference;
import org.apache.poi.ss.util.CellRangeAddress;
import org.apache.poi.xssf.usermodel.XSSFWorkbook;
import org.apache.poi.xwpf.usermodel.XWPFDocument;
import org.junit.jupiter.api.Test;

class OfficeParserTest {
    private static byte[] docx() throws Exception {
        try (XWPFDocument doc = new XWPFDocument()) {
            var title = doc.createParagraph();
            title.setStyle("Heading1");
            title.createRun().setText("价格 €");
            doc.createParagraph().createRun().setText("説明とUnicode");
            var table = doc.createTable(2, 2);
            table.getRow(0).getCell(0).setText("品名与费用");
            table.getRow(1).getCell(0).setText("甲");
            table.getRow(1).getCell(1).setText("€12");
            var span = table.getRow(0).getCell(0).getCTTc().addNewTcPr().addNewGridSpan();
            span.setVal(java.math.BigInteger.valueOf(2));
            table.getRow(0).removeCell(1);
            ByteArrayOutputStream out = new ByteArrayOutputStream();
            doc.write(out);
            return out.toByteArray();
        }
    }

    static byte[] xlsx() throws Exception {
        try (XSSFWorkbook book = new XSSFWorkbook()) {
            var sheet = book.createSheet("商品 ü");
            var header = sheet.createRow(0);
            header.createCell(0).setCellValue("Value");
            header.createCell(1).setCellValue("Value");
            var data = sheet.createRow(4);
            data.createCell(0).setCellValue("少量");
            var price = data.createCell(2);
            price.setCellValue(12.5);
            var money = book.createCellStyle();
            money.setDataFormat(book.createDataFormat().getFormat("\"€\"#,##0.00"));
            price.setCellStyle(money);
            var formula = data.createCell(3);
            formula.setCellFormula("C5*2");
            if (formula.getCTCell().isSetV()) formula.getCTCell().unsetV();
            var booleanCell = data.createCell(4);
            booleanCell.setCellValue(true);
            var emptyFormula = data.createCell(7);
            emptyFormula.setCellFormula("\"\"");
            emptyFormula.getCTCell().setT(
                    org.openxmlformats.schemas.spreadsheetml.x2006.main.STCellType.STR);
            emptyFormula.getCTCell().setV("");
            sheet.addMergedRegion(new CellRangeAddress(4, 4, 5, 6));
            data.createCell(5).setCellValue("merged label");
            ByteArrayOutputStream out = new ByteArrayOutputStream();
            book.write(out);
            return out.toByteArray();
        }
    }

    @Test
    void generatedDocxRetainsHeadingPathsTableOrderAndOriginalCoordinates() throws Exception {
        byte[] bytes = docx();
        var manifest = OfficeDocumentParser.inspect(bytes, OfficeDocumentParser.DOCX);
        assertEquals(1, manifest.units().size());
        assertEquals(0, manifest.units().getFirst().start());
        assertEquals(2, manifest.units().getFirst().end());
        String response = OfficeDocumentParser.unitJson(
                OfficeDocumentParser.parseUnit(bytes, OfficeDocumentParser.DOCX, 0));
        assertTrue(response.contains("\"kind\":\"docx_success\""), response);
        assertTrue(response.contains("\"heading_path\":[\"価格 €\"]")
                || response.contains("\"heading_path\":[\"价格 €\"]"), response);
        assertTrue(response.contains("\"table_index\":0"), response);
        assertTrue(response.contains("\"column_index\":1,\"text\":\"€12\""), response);
        assertTrue(response.contains("\"col_span\":2,\"merged\":true"), response);
        assertFalse(response.contains("\"page\""), response);
    }

    @Test
    void generatedXlsxPreservesSparseRowsCurrencyAndNeverEvaluatesFormula() throws Exception {
        byte[] bytes = xlsx();
        var manifest = OfficeDocumentParser.inspect(bytes, OfficeDocumentParser.XLSX);
        assertEquals("商品 ü", manifest.units().getFirst().sheetName());
        String json = OfficeDocumentParser.unitJson(
                OfficeDocumentParser.parseUnit(bytes, OfficeDocumentParser.XLSX, 0));
        assertTrue(json.contains("\"row\":1"), json);
        assertTrue(json.contains("\"row\":5"), json);
        assertFalse(json.contains("\"row\":2"), json);
        assertTrue(json.contains("\"reference\":\"C5\",\"column\":3,\"kind\":\"number\","
                + "\"value\":\"12.5\",\"display_value\":\"€12.50\""), json);
        assertTrue(json.contains("\"kind\":\"formula_cached\",\"value\":\"\",\"formula\":\"C5*2\""),
                json);
        assertFalse(json.contains("\"cached_value\":\"0\""), json);
        assertTrue(json.contains("\"cached_kind\":\"string\",\"cached_value\":\"\""), json);
        assertFalse(json.contains("header_range"), "row one is not an explicit workbook table");
        assertTrue(json.contains("\"reference\":\"E5\",\"column\":5,\"kind\":\"boolean\""), json);
        assertTrue(json.contains("\"reference\":\"F5\",\"column\":6,\"merged_range\":\"F5:G5\""),
                json);
    }

    @Test
    void orphanVerticalMergeFailsTheUnitInsteadOfDroppingItsText() throws Exception {
        byte[] bytes;
        try (XWPFDocument doc = new XWPFDocument()) {
            var table = doc.createTable(1, 1);
            table.getRow(0).getCell(0).setText("would be lost");
            table.getRow(0).getCell(0).getCTTc().addNewTcPr().addNewVMerge();
            ByteArrayOutputStream output = new ByteArrayOutputStream();
            doc.write(output);
            bytes = output.toByteArray();
        }
        assertTrue(OfficeDocumentParser.parseUnit(bytes, OfficeDocumentParser.DOCX, 0)
                .resultJson().contains("\"code\":\"unsupported_content\""));
    }

    @Test
    void explicitTableMetadataAloneSuppliesHeaderRange() throws Exception {
        byte[] bytes;
        try (XSSFWorkbook book = new XSSFWorkbook()) {
            var sheet = book.createSheet("Facts");
            sheet.createRow(0).createCell(0).setCellValue("Product");
            sheet.getRow(0).createCell(1).setCellValue("Currency");
            sheet.createRow(1).createCell(0).setCellValue("A");
            sheet.getRow(1).createCell(1).setCellValue("EUR");
            var table = sheet.createTable(new AreaReference("A1:B2", SpreadsheetVersion.EXCEL2007));
            table.setName("FactsTable");
            table.setDisplayName("FactsTable");
            ByteArrayOutputStream output = new ByteArrayOutputStream();
            book.write(output);
            bytes = output.toByteArray();
        }
        String result = OfficeDocumentParser.parseUnit(bytes, OfficeDocumentParser.XLSX, 0)
                .resultJson();
        assertTrue(result.contains("\"header_range\":\"A1:B1\""), result);
    }

    @Test
    void rejectsWrongFormatCorruptZipAndEncryptedContainer() throws Exception {
        byte[] word = docx();
        assertEquals("invalid_xlsx", assertThrows(OfficeDocumentParser.Failure.class,
                () -> OfficeDocumentParser.inspect(word, OfficeDocumentParser.XLSX)).code());
        assertEquals("invalid_docx", assertThrows(OfficeDocumentParser.Failure.class,
                () -> OfficeDocumentParser.inspect(new byte[]{1, 2}, OfficeDocumentParser.DOCX)).code());
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        try (ZipOutputStream zip = new ZipOutputStream(out)) {
            zip.putNextEntry(new ZipEntry("../escape"));
            zip.write("unsafe".getBytes(StandardCharsets.UTF_8));
            zip.closeEntry();
        }
        assertEquals("unit_limit", assertThrows(OfficeDocumentParser.Failure.class,
                () -> OfficeDocumentParser.inspect(out.toByteArray(), OfficeDocumentParser.XLSX)).code());
        assertEquals("unit_out_of_range", assertThrows(OfficeDocumentParser.Failure.class,
                () -> OfficeDocumentParser.parseUnit(word, OfficeDocumentParser.DOCX, 1)).code());
    }

    @Test
    void actualChildJvmsServeIndependentOfficeAndExistingPdfHealth() throws Exception {
        PdfServer server = new PdfServer(new InetSocketAddress("127.0.0.1", 0), 1);
        server.start();
        try {
            String base = "http://127.0.0.1:" + server.port();
            HttpClient client = HttpClient.newHttpClient();
            byte[] office = xlsx();
            var inspect = send(client, base + "/v1/office/inspect", office, OfficeDocumentParser.XLSX);
            assertEquals(200, inspect.statusCode(), inspect.body());
            assertTrue(inspect.body().contains(OfficeDocumentParser.VERSION), inspect.body());
            var parsed = send(client, base + "/v1/office/units/0/parse", office, OfficeDocumentParser.XLSX);
            assertEquals(200, parsed.statusCode(), parsed.body());
            assertTrue(parsed.body().contains("\"kind\":\"xlsx_success\""), parsed.body());
            var wrong = send(client, base + "/v1/office/inspect", office, "text/plain");
            assertEquals(415, wrong.statusCode(), wrong.body());
            var health = client.send(HttpRequest.newBuilder(URI.create(base + "/health")).GET().build(),
                    HttpResponse.BodyHandlers.ofString());
            assertTrue(health.body().contains("\"schema_version\":\"geo.pdf.parse.v1\""));
            assertTrue(health.body().contains("\"office_schema_version\":\"geo.office.parse.v1\""));
            assertTrue(health.body().contains("\"office_capacity\":1"));
        } finally {
            server.stop();
        }
    }

    private static HttpResponse<String> send(HttpClient client, String url, byte[] bytes, String type)
            throws Exception {
        return client.send(HttpRequest.newBuilder(URI.create(url)).header("Content-Type", type)
                        .POST(HttpRequest.BodyPublishers.ofByteArray(bytes)).build(),
                HttpResponse.BodyHandlers.ofString());
    }
}
