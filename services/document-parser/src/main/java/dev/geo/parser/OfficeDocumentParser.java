package dev.geo.parser;

import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Locale;
import java.util.Set;
import java.util.zip.ZipEntry;
import java.util.zip.ZipInputStream;
import org.apache.poi.openxml4j.opc.OPCPackage;
import org.apache.poi.openxml4j.opc.PackageRelationship;
import org.apache.poi.openxml4j.opc.TargetMode;
import org.apache.poi.openxml4j.util.ZipSecureFile;
import org.apache.poi.poifs.filesystem.POIFSFileSystem;
import org.apache.poi.ss.usermodel.Cell;
import org.apache.poi.ss.usermodel.CellType;
import org.apache.poi.ss.usermodel.DataFormatter;
import org.apache.poi.ss.usermodel.DateUtil;
import org.apache.poi.ss.usermodel.Row;
import org.apache.poi.ss.util.CellReference;
import org.apache.poi.xssf.usermodel.XSSFCell;
import org.apache.poi.xssf.usermodel.XSSFTable;
import org.apache.poi.xssf.usermodel.XSSFWorkbook;
import org.apache.poi.xwpf.usermodel.IBodyElement;
import org.apache.poi.xwpf.usermodel.XWPFDocument;
import org.apache.poi.xwpf.usermodel.XWPFParagraph;
import org.apache.poi.xwpf.usermodel.XWPFTable;
import org.apache.poi.xwpf.usermodel.XWPFTableCell;
import org.apache.poi.xwpf.usermodel.XWPFTableRow;
import org.openxmlformats.schemas.wordprocessingml.x2006.main.CTTcPr;

/** Format-aware OOXML inspection and bounded unit extraction, never formula evaluation. */
final class OfficeDocumentParser {
    static final String SCHEMA = "geo.office.parse.v1";
    static final String VERSION = "poi-5.4.1_ooxml-struct-v1";
    static final String DOCX = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
    static final String XLSX = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
    private static final int MAX_INPUT = PdfDocumentParser.MAX_INPUT;
    private static final int MAX_UNITS = 20_000;
    private static final int MAX_JSON_BYTES = 4 * 1024 * 1024;
    private static final long MAX_EXPANDED = 256L * 1024 * 1024;

    record Unit(int id, int sheetIndex, String sheetName, int start, int end) {}
    record Inspection(String sha256, String mediaType, List<Unit> units) {}
    record UnitResult(String sha256, String mediaType, String resultJson) {}
    static final class Failure extends Exception {
        private final String code;
        Failure(String code) { super(code); this.code = code; }
        String code() { return code; }
    }

    private OfficeDocumentParser() {}

    static Inspection inspect(byte[] bytes, String declaredMediaType) throws Failure {
        validateZip(bytes, declaredMediaType);
        try (OPCPackage pkg = OPCPackage.open(new ByteArrayInputStream(bytes))) {
            rejectExternalRelationships(pkg);
            List<Unit> units = new ArrayList<>();
            if (DOCX.equals(declaredMediaType)) {
                try (XWPFDocument document = new XWPFDocument(pkg)) {
                    int count = document.getBodyElements().size();
                    if (count == 0) throw new Failure("invalid_docx");
                    for (int start = 0; start < count; start += 64) {
                        units.add(new Unit(units.size(), 0, null, start, Math.min(count - 1, start + 63)));
                        if (units.size() > MAX_UNITS) throw new Failure("unit_limit");
                    }
                }
            } else {
                try (XSSFWorkbook workbook = new XSSFWorkbook(pkg)) {
                    if (workbook.getNumberOfSheets() == 0) throw new Failure("invalid_xlsx");
                    for (int index = 0; index < workbook.getNumberOfSheets(); index++) {
                        var sheet = workbook.getSheetAt(index);
                        int start = -1, previous = -1;
                        for (Row row : sheet) {
                            int number = row.getRowNum() + 1;
                            if (start < 0) start = number;
                            if (number - start >= 128) {
                                units.add(new Unit(units.size(), index, sheet.getSheetName(), start, previous));
                                start = number;
                            }
                            previous = number;
                            if (units.size() > MAX_UNITS) throw new Failure("unit_limit");
                        }
                        if (start >= 0)
                            units.add(new Unit(units.size(), index, sheet.getSheetName(), start, previous));
                        if (units.size() > MAX_UNITS) throw new Failure("unit_limit");
                    }
                    if (units.isEmpty()) throw new Failure("invalid_xlsx");
                }
            }
            return new Inspection(sha256(bytes), declaredMediaType, List.copyOf(units));
        } catch (Failure ex) {
            throw ex;
        } catch (Exception | LinkageError ex) {
            throw new Failure(invalidCode(declaredMediaType));
        }
    }

    static UnitResult parseUnit(byte[] bytes, String mediaType, int unitId) throws Failure {
        Inspection inspected = inspect(bytes, mediaType);
        if (unitId < 0 || unitId >= inspected.units().size()) throw new Failure("unit_out_of_range");
        Unit unit = inspected.units().get(unitId);
        String result;
        try (OPCPackage pkg = OPCPackage.open(new ByteArrayInputStream(bytes))) {
            rejectExternalRelationships(pkg);
            if (DOCX.equals(mediaType)) {
                try (XWPFDocument doc = new XWPFDocument(pkg)) {
                    result = docxResult(doc, unit);
                }
            } else {
                try (XSSFWorkbook book = new XSSFWorkbook(pkg)) {
                    result = xlsxResult(book, unit);
                }
            }
        } catch (Failure ex) {
            result = "{\"kind\":\"failure\",\"unit_id\":" + unitId + ",\"code\":"
                    + quote(ex.code()) + "}";
        } catch (Exception | LinkageError ex) {
            result = "{\"kind\":\"failure\",\"unit_id\":" + unitId
                    + ",\"code\":\"parse_failed\"}";
        }
        if (result.getBytes(StandardCharsets.UTF_8).length > MAX_JSON_BYTES) {
            result = "{\"kind\":\"failure\",\"unit_id\":" + unitId
                    + ",\"code\":\"unit_limit\"}";
        }
        return new UnitResult(inspected.sha256(), mediaType, result);
    }

    private static String docxResult(XWPFDocument doc, Unit unit) throws Failure {
        StringBuilder json = new StringBuilder("{\"kind\":\"docx_success\",\"unit_id\":")
                .append(unit.id()).append(",\"elements\":[");
        List<String> headings = new ArrayList<>();
        int paragraphIndex = 0, tableIndex = 0;
        List<IBodyElement> body = doc.getBodyElements();
        // Reconstruct heading context and ordinal positions from preceding blocks.
        for (int i = 0; i <= unit.end(); i++) {
            IBodyElement element = body.get(i);
            if (element instanceof XWPFParagraph paragraph) {
                int level = headingLevel(paragraph);
                if (level > 0) {
                    while (headings.size() >= level) headings.remove(headings.size() - 1);
                    while (headings.size() < level - 1) headings.add("");
                    headings.add(paragraph.getText());
                }
                if (i >= unit.start()) {
                    if (json.charAt(json.length() - 1) != '[') json.append(',');
                    json.append("{\"kind\":\"paragraph\",\"body_element_index\":").append(i)
                            .append(",\"paragraph_index\":").append(paragraphIndex)
                            .append(",\"heading_path\":").append(stringArray(headings))
                            .append(",\"text\":").append(quote(paragraph.getText()))
                            .append('}');
                }
                paragraphIndex++;
            } else if (element instanceof XWPFTable table) {
                if (i >= unit.start()) {
                    if (json.charAt(json.length() - 1) != '[') json.append(',');
                    json.append("{\"kind\":\"table\",\"body_element_index\":").append(i)
                            .append(",\"table_index\":").append(tableIndex)
                            .append(",\"heading_path\":").append(stringArray(headings))
                            .append(",\"rows\":[");
                    appendTable(json, table);
                    json.append("]}");
                }
                tableIndex++;
            } else if (i >= unit.start()) {
                throw new Failure("unsupported_content");
            }
        }
        return json.append("]}").toString();
    }

    private static int headingLevel(XWPFParagraph paragraph) {
        String style = paragraph.getStyle();
        if (style == null) return 0;
        java.util.regex.Matcher matcher = java.util.regex.Pattern
                .compile("(?i)^heading\\s*([1-9])$").matcher(style);
        return matcher.matches() ? Integer.parseInt(matcher.group(1)) : 0;
    }

    private static void appendTable(StringBuilder json, XWPFTable table) throws Failure {
        // Vertical merges need a complete rectangular grid to recover spans exactly.
        List<List<XWPFTableCell>> grid = new ArrayList<>();
        for (XWPFTableRow row : table.getRows()) {
            if (row.getCtRow().selectPath("declare namespace w='http://schemas.openxmlformats.org/"
                    + "wordprocessingml/2006/main'; .//w:gridBefore | .//w:gridAfter").length != 0)
                throw new Failure("unsupported_content");
            List<XWPFTableCell> cells = new ArrayList<>();
            for (XWPFTableCell cell : row.getTableCells()) {
                if (!cell.getTables().isEmpty()) throw new Failure("unsupported_content");
                if (cell.getCTTc().selectPath("declare namespace w='http://schemas.openxmlformats.org/"
                        + "wordprocessingml/2006/main'; .//w:hMerge").length != 0)
                    throw new Failure("unsupported_content");
                CTTcPr properties = cell.getCTTc().getTcPr();
                int span = properties != null && properties.isSetGridSpan()
                        ? properties.getGridSpan().getVal().intValue() : 1;
                if (span < 1 || span > 256 || cells.size() + span > 256)
                    throw new Failure("unsupported_content");
                cells.add(cell);
                for (int i = 1; i < span; i++) cells.add(null);
            }
            grid.add(cells);
        }
        if (grid.size() > 4_096) throw new Failure("unit_limit");
        for (int r = 0; r < grid.size(); r++) {
            if (r > 0) json.append(',');
            json.append("{\"row_index\":").append(r).append(",\"cells\":[");
            List<XWPFTableCell> cells = grid.get(r);
            boolean emitted = false;
            for (int c = 0; c < cells.size(); c++) {
                XWPFTableCell cell = cells.get(c);
                if (cell == null) continue;
                CTTcPr props = cell.getCTTc().getTcPr();
                int colSpan = props != null && props.isSetGridSpan()
                        ? props.getGridSpan().getVal().intValue() : 1;
                boolean continuation = props != null && props.isSetVMerge()
                        && (props.getVMerge().getVal() == null
                            || props.getVMerge().getVal().toString().equals("continue"));
                if (continuation) {
                    // A continuation without a provable preceding start is not evidence.
                    if (r == 0 || c >= grid.get(r - 1).size()
                            || grid.get(r - 1).get(c) == null || !cell.getText().isBlank())
                        throw new Failure("unsupported_content");
                    CTTcPr prior = grid.get(r - 1).get(c).getCTTc().getTcPr();
                    int priorSpan = prior != null && prior.isSetGridSpan()
                            ? prior.getGridSpan().getVal().intValue() : 1;
                    if (prior == null || !prior.isSetVMerge() || priorSpan != colSpan)
                        throw new Failure("unsupported_content");
                    continue;
                }
                int rowSpan = 1;
                if (props != null && props.isSetVMerge()) {
                    for (int next = r + 1; next < grid.size(); next++) {
                        if (c >= grid.get(next).size()) break;
                        XWPFTableCell follower = grid.get(next).get(c);
                        if (follower == null) break;
                        CTTcPr followProps = follower.getCTTc().getTcPr();
                        if (followProps == null || !followProps.isSetVMerge()
                                || (followProps.getVMerge().getVal() != null
                                    && !followProps.getVMerge().getVal().toString().equals("continue")))
                            break;
                        int followerSpan = followProps.isSetGridSpan()
                                ? followProps.getGridSpan().getVal().intValue() : 1;
                        if (followerSpan != colSpan) throw new Failure("unsupported_content");
                        rowSpan++;
                    }
                }
                if (emitted) json.append(',');
                json.append("{\"column_index\":").append(c)
                        .append(",\"text\":").append(quote(cell.getText()))
                        .append(",\"row_span\":").append(rowSpan)
                        .append(",\"col_span\":").append(colSpan)
                        .append(",\"merged\":").append(rowSpan > 1 || colSpan > 1).append('}');
                emitted = true;
            }
            json.append("]}");
        }
    }

    private static String xlsxResult(XSSFWorkbook book, Unit unit) throws Failure {
        var sheet = book.getSheetAt(unit.sheetIndex());
        var mergedRanges = sheet.getMergedRegions();
        if (mergedRanges.size() > 2_048) throw new Failure("unit_limit");
        DataFormatter formatter = new DataFormatter(Locale.ROOT, false);
        StringBuilder json = new StringBuilder("{\"kind\":\"xlsx_success\",\"unit_id\":")
                .append(unit.id()).append(",\"rows\":[");
        int totalCells = 0;
        for (Row row : sheet) {
            int rowNumber = row.getRowNum() + 1;
            if (rowNumber < unit.start() || rowNumber > unit.end()) continue;
            if (json.charAt(json.length() - 1) != '[') json.append(',');
            json.append("{\"row\":").append(rowNumber).append(",\"cells\":[");
            for (Cell cell : row) {
                if (cell.getCellType() == CellType.BLANK) continue;
                if (++totalCells > 20_000) throw new Failure("unit_limit");
                if (json.charAt(json.length() - 1) != '[') json.append(',');
                String mergedRange = null;
                for (var range : mergedRanges) {
                    if (range.isInRange(cell.getRowIndex(), cell.getColumnIndex())) {
                        if (mergedRange != null) throw new Failure("unsupported_content");
                        mergedRange = range.formatAsString();
                    }
                }
                json.append(cellJson((XSSFCell) cell, formatter, mergedRange));
            }
            json.append(']');
            String headerRange = null;
            for (XSSFTable table : sheet.getTables()) {
                if (table.getHeaderRowCount() == 0) continue;
                var range = table.getCellReferences();
                if (range == null) continue;
                var first = range.getFirstCell();
                var last = range.getLastCell();
                if (row.getRowNum() >= first.getRow() && row.getRowNum() <= last.getRow()) {
                    String candidate = new CellReference(first.getRow(), first.getCol()).formatAsString()
                            + ":" + new CellReference(first.getRow() + table.getHeaderRowCount() - 1,
                                    last.getCol()).formatAsString();
                    if (headerRange != null && !headerRange.equals(candidate))
                        throw new Failure("unsupported_content");
                    headerRange = candidate;
                }
            }
            if (headerRange != null) json.append(",\"header_range\":").append(quote(headerRange));
            json.append('}');
        }
        return json.append("]}").toString();
    }

    private static String cellJson(XSSFCell cell, DataFormatter formatter, String mergedRange) {
        StringBuilder json = new StringBuilder("{\"reference\":")
                .append(quote(cell.getAddress().formatAsString()))
                .append(",\"column\":").append(cell.getColumnIndex() + 1);
        if (mergedRange != null) json.append(",\"merged_range\":").append(quote(mergedRange));
        CellType type = cell.getCellType();
        if (type == CellType.FORMULA) {
            String formula = cell.getCellFormula();
            json.append(",\"kind\":\"formula_cached\",\"value\":")
                    .append(quote(cell.getCTCell().isSetV() ? cell.getCTCell().getV() : ""))
                    .append(",\"formula\":").append(quote(formula));
            // OOXML without <v> has no cached value: POI otherwise fabricates numeric 0.
            // An explicit empty cached STRING is meaningful; an empty numeric <v/>
            // does not prove the numeric zero that POI otherwise reports.
            boolean hasCache = cell.getCTCell().isSetV()
                    && (!cell.getCTCell().getV().isEmpty()
                        || cell.getCachedFormulaResultType() == CellType.STRING);
            if (hasCache) {
                CellType cached = cell.getCachedFormulaResultType();
                if (cached != CellType.BLANK) {
                    String kind = cellKind(cell, cached);
                    String value = rawValue(cell, cached);
                    json.append(",\"cached_kind\":").append(quote(kind))
                            .append(",\"cached_value\":").append(quote(value))
                            .append(",\"display_value\":")
                            .append(quote(displayValue(cell, cached, formatter)));
                }
            }
        } else {
            String kind = cellKind(cell, type);
            json.append(",\"kind\":").append(quote(kind))
                    .append(",\"value\":").append(quote(rawValue(cell, type)))
                    .append(",\"display_value\":")
                    .append(quote(displayValue(cell, type, formatter)));
        }
        return json.append('}').toString();
    }

    private static String cellKind(XSSFCell cell, CellType type) {
        return switch (type) {
            case STRING -> "string";
            case NUMERIC -> DateUtil.isCellDateFormatted(cell) ? "date" : "number";
            case BOOLEAN -> "boolean";
            case ERROR -> "error";
            default -> "error";
        };
    }

    private static String rawValue(XSSFCell cell, CellType type) {
        return switch (type) {
            case STRING -> cell.getStringCellValue();
            case NUMERIC -> cell.getCTCell().getV();
            case BOOLEAN -> Boolean.toString(cell.getBooleanCellValue());
            case ERROR -> Byte.toString(cell.getErrorCellValue());
            default -> "";
        };
    }

    private static String displayValue(XSSFCell cell, CellType type, DataFormatter formatter) {
        if (type == CellType.NUMERIC) {
            return formatter.formatRawCellContents(cell.getNumericCellValue(),
                    cell.getCellStyle().getDataFormat(),
                    cell.getCellStyle().getDataFormatString());
        }
        return rawValue(cell, type);
    }

    private static void validateZip(byte[] bytes, String mediaType) throws Failure {
        if (!DOCX.equals(mediaType) && !XLSX.equals(mediaType))
            throw new Failure("unsupported_media_type");
        if (bytes.length > MAX_INPUT) throw new Failure("input_too_large");
        if (bytes.length >= 8 && (bytes[0] & 0xff) == 0xd0
                && (bytes[1] & 0xff) == 0xcf
                && (bytes[2] & 0xff) == 0x11
                && (bytes[3] & 0xff) == 0xe0) {
            try (POIFSFileSystem ole = new POIFSFileSystem(new ByteArrayInputStream(bytes))) {
                if (ole.getRoot().hasEntry("EncryptionInfo")
                        && ole.getRoot().hasEntry("EncryptedPackage"))
                    throw new Failure("encrypted_office");
            } catch (Failure ex) {
                throw ex;
            } catch (IOException | RuntimeException ex) {
                throw new Failure(invalidCode(mediaType));
            }
        }
        if (bytes.length < 4 || bytes[0] != 'P' || bytes[1] != 'K')
            throw new Failure(invalidCode(mediaType));
        ZipSecureFile.setMinInflateRatio(0.01);
        ZipSecureFile.setMaxEntrySize(64L * 1024 * 1024);
        ZipSecureFile.setMaxTextSize(MAX_EXPANDED);
        boolean contentTypes = false, rootRels = false, main = false;
        long expanded = 0;
        int count = 0;
        Set<String> names = new HashSet<>();
        try (ZipInputStream archive = new ZipInputStream(new ByteArrayInputStream(bytes))) {
            ZipEntry entry;
            byte[] buffer = new byte[8_192];
            while ((entry = archive.getNextEntry()) != null) {
                if (++count > 4_096 || !names.add(entry.getName())
                        || entry.getName().startsWith("/")
                        || entry.getName().contains("\\")
                        || entry.getName().contains(".."))
                    throw new Failure("unit_limit");
                if (entry.getName().equals("[Content_Types].xml")) contentTypes = true;
                if (entry.getName().equals("_rels/.rels")) rootRels = true;
                if (entry.getName().equals(DOCX.equals(mediaType)
                        ? "word/document.xml" : "xl/workbook.xml")) main = true;
                int read;
                long entrySize = 0;
                while ((read = archive.read(buffer)) != -1) {
                    expanded += read;
                    entrySize += read;
                    if (expanded > MAX_EXPANDED || entrySize > 64L * 1024 * 1024)
                        throw new Failure("unit_limit");
                }
            }
        } catch (Failure ex) {
            throw ex;
        } catch (IOException | RuntimeException ex) {
            throw new Failure(invalidCode(mediaType));
        }
        if (!contentTypes || !rootRels || !main) throw new Failure(invalidCode(mediaType));
    }

    private static void rejectExternalRelationships(OPCPackage pkg) throws Failure {
        try {
            for (PackageRelationship relationship : pkg.getRelationships()) {
                if (relationship.getTargetMode() == TargetMode.EXTERNAL)
                    throw new Failure("unsupported_content");
            }
            for (var part : pkg.getParts()) {
                if (part.getPartName().getName().endsWith(".rels")) continue;
                for (PackageRelationship relationship : part.getRelationships()) {
                    if (relationship.getTargetMode() == TargetMode.EXTERNAL)
                        throw new Failure("unsupported_content");
                }
            }
            if (pkg.containPart(org.apache.poi.openxml4j.opc.PackagingURIHelper
                    .createPartName("/EncryptionInfo")))
                throw new Failure("encrypted_office");
        } catch (Failure ex) {
            throw ex;
        } catch (Exception ex) {
            throw new Failure("unsupported_content");
        }
    }

    private static String invalidCode(String mediaType) {
        return DOCX.equals(mediaType) ? "invalid_docx" : "invalid_xlsx";
    }

    private static String sha256(byte[] bytes) {
        try {
            return java.util.HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(bytes));
        } catch (NoSuchAlgorithmException ex) {
            throw new AssertionError(ex);
        }
    }

    static String inspectJson(Inspection inspection) {
        StringBuilder json = new StringBuilder("{\"schema_version\":").append(quote(SCHEMA))
                .append(",\"input_sha256\":").append(quote(inspection.sha256()))
                .append(",\"parser_version\":").append(quote(VERSION))
                .append(",\"media_type\":").append(quote(inspection.mediaType()))
                .append(",\"document\":{\"format\":")
                .append(quote(DOCX.equals(inspection.mediaType()) ? "docx" : "xlsx"))
                .append(",\"units\":[");
        for (Unit unit : inspection.units()) {
            if (json.charAt(json.length() - 1) != '[') json.append(',');
            json.append("{\"unit_id\":").append(unit.id());
            if (DOCX.equals(inspection.mediaType())) {
                json.append(",\"start_body_element\":").append(unit.start())
                        .append(",\"end_body_element\":").append(unit.end());
            } else {
                json.append(",\"sheet_name\":").append(quote(unit.sheetName()))
                        .append(",\"sheet_index\":").append(unit.sheetIndex())
                        .append(",\"start_row\":").append(unit.start())
                        .append(",\"end_row\":").append(unit.end());
            }
            json.append('}');
        }
        return json.append("]}}").toString();
    }

    static String unitJson(UnitResult result) {
        return "{\"schema_version\":" + quote(SCHEMA)
                + ",\"input_sha256\":" + quote(result.sha256())
                + ",\"parser_version\":" + quote(VERSION)
                + ",\"media_type\":" + quote(result.mediaType())
                + ",\"result\":" + result.resultJson() + "}";
    }

    private static String stringArray(List<String> values) {
        StringBuilder out = new StringBuilder("[");
        for (String value : values) {
            if (out.length() > 1) out.append(',');
            out.append(quote(value));
        }
        return out.append(']').toString();
    }

    static String quote(String input) {
        if (input == null) input = "";
        StringBuilder out = new StringBuilder(input.length() + 2).append('"');
        for (int i = 0; i < input.length(); i++) {
            char c = input.charAt(i);
            switch (c) {
                case '"' -> out.append("\\\"");
                case '\\' -> out.append("\\\\");
                case '\n' -> out.append("\\n");
                case '\r' -> out.append("\\r");
                case '\t' -> out.append("\\t");
                default -> {
                    if (c < 0x20) out.append(String.format("\\u%04x", (int) c));
                    else out.append(c);
                }
            }
        }
        return out.append('"').toString();
    }
}
