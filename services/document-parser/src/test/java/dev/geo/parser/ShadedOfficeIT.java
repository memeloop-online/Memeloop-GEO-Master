package dev.geo.parser;

import static org.junit.jupiter.api.Assertions.*;

import java.net.ServerSocket;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.file.Path;
import java.time.Duration;
import java.util.concurrent.TimeUnit;
import org.junit.jupiter.api.Test;

/** Exercise the packaged distribution, including its disposable worker classpath. */
class ShadedOfficeIT {
    @Test
    void shippedJarServesRealOoXmlWithoutChangingPdfHealthIdentity() throws Exception {
        int port;
        try (ServerSocket reserved = new ServerSocket(0)) {
            port = reserved.getLocalPort();
        }
        String java = Path.of(System.getProperty("java.home"), "bin",
                System.getProperty("os.name").startsWith("Windows") ? "java.exe" : "java").toString();
        ProcessBuilder launcher = new ProcessBuilder(java, "-Dlog4j2.statusLoggerLevel=OFF",
                "-jar", System.getProperty("shadedJarPath"))
                .redirectError(ProcessBuilder.Redirect.DISCARD)
                .redirectOutput(ProcessBuilder.Redirect.DISCARD);
        launcher.environment().put("GEO_PDF_BIND", "127.0.0.1");
        launcher.environment().put("GEO_PDF_PORT", Integer.toString(port));
        Process process = launcher.start();
        try {
            HttpClient client = HttpClient.newBuilder().connectTimeout(Duration.ofSeconds(2)).build();
            String base = "http://127.0.0.1:" + port;
            HttpResponse<String> health = null;
            for (int attempt = 0; attempt < 60; attempt++) {
                if (!process.isAlive()) fail("shaded server failed before becoming healthy");
                try {
                    health = client.send(HttpRequest.newBuilder(URI.create(base + "/health"))
                                    .timeout(Duration.ofSeconds(2)).GET().build(),
                            HttpResponse.BodyHandlers.ofString());
                    break;
                } catch (java.net.ConnectException ignored) {
                    Thread.sleep(100);
                }
            }
            assertNotNull(health, "shaded server must start");
            assertEquals(200, health.statusCode());
            assertTrue(health.body().contains("\"schema_version\":\"geo.pdf.parse.v1\""));
            assertTrue(health.body().contains("\"office_parser_version\":\""
                    + OfficeDocumentParser.VERSION + "\""));
            byte[] workbook = OfficeParserTest.xlsx();
            var inspected = client.send(HttpRequest.newBuilder(URI.create(base + "/v1/office/inspect"))
                            .header("Content-Type", OfficeDocumentParser.XLSX)
                            .timeout(Duration.ofSeconds(25))
                            .POST(HttpRequest.BodyPublishers.ofByteArray(workbook)).build(),
                    HttpResponse.BodyHandlers.ofString());
            assertEquals(200, inspected.statusCode(), inspected.body());
            var parsed = client.send(HttpRequest.newBuilder(URI.create(base + "/v1/office/units/0/parse"))
                            .header("Content-Type", OfficeDocumentParser.XLSX)
                            .timeout(Duration.ofSeconds(25))
                            .POST(HttpRequest.BodyPublishers.ofByteArray(workbook)).build(),
                    HttpResponse.BodyHandlers.ofString());
            assertEquals(200, parsed.statusCode(), parsed.body());
            assertTrue(parsed.body().contains("\"merged_range\":\"F5:G5\""), parsed.body());
            assertTrue(parsed.body().contains("\"kind\":\"formula_cached\""), parsed.body());
        } finally {
            process.destroyForcibly();
            assertTrue(process.waitFor(5, TimeUnit.SECONDS), "test server must terminate");
        }
    }
}
