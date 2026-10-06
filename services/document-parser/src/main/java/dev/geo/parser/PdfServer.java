package dev.geo.parser;

import com.sun.net.httpserver.HttpExchange;
import com.sun.net.httpserver.HttpServer;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.net.InetSocketAddress;
import java.nio.charset.StandardCharsets;
import java.util.Locale;
import java.util.concurrent.Executors;
import java.util.concurrent.Semaphore;

/** Internal-only HTTP transport. It emits no request, filename or parser exception logs. */
public final class PdfServer {
    private final HttpServer server;
    private final Semaphore slots;
    private final int capacity;
    private static final String PAGE_PREFIX = "/v1/pdf/pages/";
    private static final String OFFICE_PREFIX = "/v1/office/units/";

    public PdfServer(InetSocketAddress address, int maxConcurrent) throws IOException {
        if (maxConcurrent < 1 || maxConcurrent > 64)
            throw new IllegalArgumentException("invalid concurrency");
        silenceLibraries();
        server = HttpServer.create(address, 32);
        capacity = maxConcurrent;
        slots = new Semaphore(maxConcurrent);
        server.createContext("/", this::handle);
        server.setExecutor(Executors.newVirtualThreadPerTaskExecutor());
    }

    public void start() { server.start(); }
    public void stop() { server.stop(0); }
    public int port() { return server.getAddress().getPort(); }

    public static void main(String[] args) throws IOException {
        silenceLibraries();
        if (args.length > 0) {
            WorkerProcess.runWorker(args);
            return;
        }
        String host = System.getenv().getOrDefault("GEO_PDF_BIND", "127.0.0.1");
        int port = boundedEnv("GEO_PDF_PORT", 8080, 1, 65535);
        int concurrency = boundedEnv("GEO_PDF_MAX_CONCURRENT", 2, 1, 64);
        new PdfServer(new InetSocketAddress(host, port), concurrency).start();
    }

    private static void silenceLibraries() {
        // Commons Logging and JUL diagnostics may include PDF metadata or text.
        System.setProperty("org.apache.commons.logging.Log",
                "org.apache.commons.logging.impl.NoOpLog");
        java.util.logging.LogManager.getLogManager().reset();
    }

    private static int boundedEnv(String key, int fallback, int min, int max) {
        String raw = System.getenv(key);
        int value = raw == null ? fallback : Integer.parseInt(raw);
        if (value < min || value > max) throw new IllegalArgumentException("invalid configuration");
        return value;
    }

    private void handle(HttpExchange exchange) throws IOException {
        try {
            String path = exchange.getRequestURI().getRawPath();
            if (path.equals("/health")) {
                if (!exchange.getRequestMethod().equals("GET")) {
                    respond(exchange, 405, error("method_not_allowed"));
                } else {
                    respond(exchange, 200, "{\"schema_version\":\"" + PdfDocumentParser.SCHEMA
                            + "\",\"parser_version\":\"" + PdfDocumentParser.VERSION
                            + "\",\"capacity\":" + capacity
                            + ",\"office_schema_version\":\"" + OfficeDocumentParser.SCHEMA
                            + "\",\"office_parser_version\":\"" + OfficeDocumentParser.VERSION
                            + "\",\"office_capacity\":" + capacity + "}");
                }
                return;
            }
            if (path.equals("/v1/office/inspect") || path.startsWith(OFFICE_PREFIX)) {
                handleOffice(exchange, path);
                return;
            }
            boolean inspect = path.equals("/v1/pdf/inspect");
            String pagePart = path.startsWith(PAGE_PREFIX)
                    ? path.substring(PAGE_PREFIX.length()) : "";
            if (!inspect && !pagePart.matches("[1-9][0-9]{0,8}/parse")) {
                respond(exchange, 404, error("not_found"));
                return;
            }
            if (!exchange.getRequestMethod().equals("POST")) {
                respond(exchange, 405, error("method_not_allowed"));
                return;
            }
            String contentType = exchange.getRequestHeaders().getFirst("Content-Type");
            if (contentType == null || !contentType.toLowerCase(Locale.ROOT).equals("application/pdf")) {
                respond(exchange, 415, error("unsupported_media_type"));
                return;
            }
            if (!slots.tryAcquire()) {
                respond(exchange, 503, error("parser_busy"));
                return;
            }
            try {
                if (exchange.getRequestHeaders().getFirst("Content-Length") != null) {
                    long advertised;
                    try {
                        advertised = Long.parseLong(exchange.getRequestHeaders().getFirst("Content-Length"));
                    } catch (NumberFormatException ex) {
                        respond(exchange, 400, error("invalid_request"));
                        return;
                    }
                    if (advertised < 0) {
                        respond(exchange, 400, error("invalid_request"));
                        return;
                    }
                    if (advertised > PdfDocumentParser.MAX_INPUT) {
                        respond(exchange, 413, error("input_too_large"));
                        return;
                    }
                }
                byte[] input = readBounded(exchange);
                Integer page = inspect ? null
                        : Integer.parseInt(pagePart.substring(0, pagePart.indexOf('/')));
                var result = WorkerProcess.execute(input, page);
                if (result.error() == null) {
                    respond(exchange, 200, result.json());
                } else {
                    int status = switch (result.error()) {
                        case "parser_timeout" -> 504;
                        case "parser_failed" -> 502;
                        case "input_too_large" -> 413;
                        case "page_out_of_range" -> 404;
                        case "output_too_large", "page_limit_exceeded" -> 422;
                        default -> 400;
                    };
                    respond(exchange, status, error(result.error()));
                }
            } catch (PdfDocumentParser.ParseFailure ex) {
                int status = switch (ex.code()) {
                    case "input_too_large" -> 413;
                    case "page_out_of_range" -> 404;
                    case "output_too_large", "page_limit_exceeded" -> 422;
                    default -> 400;
                };
                respond(exchange, status, error(ex.code()));
            } finally {
                slots.release();
            }
        } catch (RuntimeException ex) {
            respond(exchange, 400, error("invalid_request"));
        } finally {
            exchange.close();
        }
    }

    private void handleOffice(HttpExchange exchange, String path) throws IOException {
        boolean inspect = path.equals("/v1/office/inspect");
        String unitPart = path.startsWith(OFFICE_PREFIX)
                ? path.substring(OFFICE_PREFIX.length()) : "";
        if (!inspect && !unitPart.matches("(0|[1-9][0-9]{0,4})/parse")) {
            respond(exchange, 404, error("not_found"));
            return;
        }
        if (!exchange.getRequestMethod().equals("POST")) {
            respond(exchange, 405, error("method_not_allowed"));
            return;
        }
        String mediaType = exchange.getRequestHeaders().getFirst("Content-Type");
        if (!OfficeDocumentParser.DOCX.equals(mediaType)
                && !OfficeDocumentParser.XLSX.equals(mediaType)) {
            respond(exchange, 415, error("unsupported_media_type"));
            return;
        }
        if (!slots.tryAcquire()) {
            respond(exchange, 503, error("parser_busy"));
            return;
        }
        try {
            String advertised = exchange.getRequestHeaders().getFirst("Content-Length");
            if (advertised != null) {
                long size;
                try {
                    size = Long.parseLong(advertised);
                } catch (NumberFormatException ex) {
                    respond(exchange, 400, error("invalid_request"));
                    return;
                }
                if (size < 0 || size > PdfDocumentParser.MAX_INPUT) {
                    respond(exchange, size < 0 ? 400 : 413,
                            error(size < 0 ? "invalid_request" : "input_too_large"));
                    return;
                }
            }
            byte[] bytes = readBounded(exchange);
            Integer unit = inspect ? null : Integer.parseInt(unitPart.substring(0, unitPart.indexOf('/')));
            var result = WorkerProcess.executeOffice(bytes, mediaType, unit);
            if (result.error() == null) {
                respond(exchange, 200, result.json());
            } else {
                int status = switch (result.error()) {
                    case "parser_timeout" -> 504;
                    case "parser_failed" -> 502;
                    case "input_too_large" -> 413;
                    case "unit_out_of_range" -> 404;
                    case "unsupported_media_type" -> 415;
                    case "unit_limit" -> 422;
                    default -> 400;
                };
                respond(exchange, status, error(result.error()));
            }
        } catch (PdfDocumentParser.ParseFailure ex) {
            respond(exchange, 413, error(ex.code()));
        } finally {
            slots.release();
        }
    }

    private static byte[] readBounded(HttpExchange exchange)
            throws IOException, PdfDocumentParser.ParseFailure {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        byte[] buffer = new byte[8192];
        int read;
        while ((read = exchange.getRequestBody().read(buffer)) != -1) {
            if (read > PdfDocumentParser.MAX_INPUT - out.size())
                throw new PdfDocumentParser.ParseFailure("input_too_large");
            out.write(buffer, 0, read);
        }
        return out.toByteArray();
    }

    static String error(String code) {
        return "{\"error\":{\"code\":\"" + code + "\",\"retryable\":"
                + (code.equals("parser_busy") || code.equals("parser_timeout")
                    || code.equals("parser_failed") ? "true" : "false") + "}}";
    }

    static String inspectJson(PdfDocumentParser.Inspection result) {
        return "{\"schema_version\":\"" + PdfDocumentParser.SCHEMA
                + "\",\"input_sha256\":\"" + result.inputSha256()
                + "\",\"parser_version\":\"" + PdfDocumentParser.VERSION
                + "\",\"page_count\":" + result.pageCount() + "}";
    }

    static String pageJson(PdfDocumentParser.PageText result) {
        return "{\"schema_version\":\"" + PdfDocumentParser.SCHEMA
                + "\",\"input_sha256\":\"" + result.inputSha256()
                + "\",\"parser_version\":\"" + PdfDocumentParser.VERSION
                + "\",\"page\":" + result.page()
                + ",\"text\":" + jsonString(result.text())
                + (result.reason() == null ? "" : ",\"reason\":\"" + result.reason() + "\"")
                + "}";
    }

    private static String jsonString(String value) {
        StringBuilder json = new StringBuilder(value.length() + 2).append('"');
        for (int index = 0; index < value.length(); index++) {
            char ch = value.charAt(index);
            switch (ch) {
                case '"' -> json.append("\\\"");
                case '\\' -> json.append("\\\\");
                case '\n' -> json.append("\\n");
                case '\r' -> json.append("\\r");
                case '\t' -> json.append("\\t");
                default -> {
                    if (ch < 0x20) json.append(String.format("\\u%04x", (int) ch));
                    else json.append(ch);
                }
            }
        }
        return json.append('"').toString();
    }

    private static void respond(HttpExchange exchange, int code, String json) throws IOException {
        byte[] bytes = json.getBytes(StandardCharsets.UTF_8);
        exchange.getResponseHeaders().set("Content-Type", "application/json; charset=utf-8");
        exchange.getResponseHeaders().set("Cache-Control", "no-store");
        exchange.sendResponseHeaders(code, bytes.length);
        exchange.getResponseBody().write(bytes);
    }
}
