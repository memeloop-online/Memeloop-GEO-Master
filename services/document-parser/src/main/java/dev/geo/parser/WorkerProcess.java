package dev.geo.parser;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicLong;
import java.util.concurrent.atomic.AtomicBoolean;

/** One disposable JVM per request. The HTTP process never parses untrusted PDF bytes. */
final class WorkerProcess {
    private static final long DEADLINE_MS = 20_000;
    private static final int MAX_WORKER_OUTPUT = 32 * 1024 * 1024;
    private static final String MAIN_CLASS = "dev.geo.parser.PdfServer";

    record Outcome(String json, String error) {}

    private WorkerProcess() {}

    static Outcome execute(byte[] pdf, Integer page) {
        return launch(pdf, page, DEADLINE_MS, MAIN_CLASS, null, null, null);
    }

    static Outcome executeOffice(byte[] bytes, String mediaType, Integer unit) {
        return launch(bytes, unit, DEADLINE_MS, MAIN_CLASS, null, null, mediaType);
    }

    // Test-only entry point: a fixed helper class, never derived from HTTP data.
    static Outcome executeProbe(String fixedMode, long timeoutMs, AtomicLong pid) {
        if (!List.of("hang", "oversize", "exit", "env").contains(fixedMode))
            throw new IllegalArgumentException("invalid test mode");
        return launch(new byte[0], null, timeoutMs,
                "dev.geo.parser.WorkerProcessProbe", fixedMode, pid, null);
    }

    private static Outcome launch(byte[] pdf, Integer page, long timeoutMs,
            String mainClass, String testMode, AtomicLong observedPid, String officeMediaType) {
        List<String> command = new ArrayList<>();
        command.add(Path.of(System.getProperty("java.home"), "bin",
                isWindows() ? "java.exe" : "java").toString());
        command.add("-Xmx256m");
        command.add("-Dlog4j2.statusLoggerLevel=OFF");
        command.add("-Djava.io.tmpdir=" + System.getProperty("java.io.tmpdir"));
        command.add("-Duser.home=" + System.getProperty("java.io.tmpdir"));
        command.add("-cp");
        command.add(runtimeClasspath());
        command.add(mainClass);
        if (testMode == null) {
            command.add("--worker");
            command.add(officeMediaType == null ? (page == null ? "inspect" : "page")
                    : (page == null ? "office-inspect" : "office-unit"));
            if (page != null) command.add(Integer.toString(page));
            if (officeMediaType != null) command.add(officeMediaType);
        } else {
            command.add(testMode);
        }
        Process process;
        try {
            ProcessBuilder builder = new ProcessBuilder(command)
                    .redirectError(ProcessBuilder.Redirect.DISCARD);
            // Do not forward unrelated service credentials, proxy variables,
            // JAVA_TOOL_OPTIONS, user home, or application configuration.
            var childEnvironment = builder.environment();
            childEnvironment.clear();
            if (isWindows()) {
                copyWindowsSystemVariable(childEnvironment, "SystemRoot");
                copyWindowsSystemVariable(childEnvironment, "WINDIR");
            }
            process = builder.start();
        } catch (IOException | RuntimeException ex) {
            return new Outcome(null, "parser_failed");
        }
        if (observedPid != null) observedPid.set(process.pid());
        var virtualExecutor = (java.util.concurrent.Executor) task -> Thread.ofVirtual().start(task);
        CompletableFuture<Boolean> writer = CompletableFuture.supplyAsync(() -> {
            try (var stream = process.getOutputStream()) {
                stream.write(pdf);
                return true;
            } catch (IOException ex) {
                return false;
            }
        }, virtualExecutor);
        AtomicBoolean oversized = new AtomicBoolean();
        CompletableFuture<byte[]> reader = CompletableFuture.supplyAsync(() -> {
            try (var stream = process.getInputStream()) {
                return readOutput(stream, process, oversized);
            } catch (IOException ex) {
                return null;
            }
        }, virtualExecutor);
        try {
            if (!process.waitFor(timeoutMs, TimeUnit.MILLISECONDS))
                return new Outcome(null, "parser_timeout");
            byte[] output = reader.get(1, TimeUnit.SECONDS);
            boolean sent = writer.get(1, TimeUnit.SECONDS);
            if (oversized.get()) return new Outcome(null, "output_too_large");
            if (!sent || output == null || process.exitValue() != 0)
                return new Outcome(null, "parser_failed");
            String body = new String(output, StandardCharsets.UTF_8);
            if (body.startsWith("OK\n") && body.length() > 3)
                return new Outcome(body.substring(3), null);
            if (body.startsWith("ERR\n") && isParserCode(body.substring(4)))
                return new Outcome(null, body.substring(4));
            return new Outcome(null, "parser_failed");
        } catch (InterruptedException ex) {
            Thread.currentThread().interrupt();
            return new Outcome(null, "parser_failed");
        } catch (ExecutionException | java.util.concurrent.TimeoutException ex) {
            return new Outcome(null, "parser_failed");
        } finally {
            // Terminate the entire untrusted parser process, including its threads.
            // Do not free a semaphore permit until it has actually exited.
            process.destroyForcibly();
            try {
                process.waitFor();
            } catch (InterruptedException ex) {
                Thread.interrupted();
                boolean exited = false;
                while (!exited) {
                    try {
                        process.waitFor();
                        exited = true;
                    } catch (InterruptedException ignored) {
                        Thread.interrupted();
                    }
                }
                Thread.currentThread().interrupt();
            }
            writer.cancel(true);
            reader.cancel(true);
        }
    }

    private static byte[] readOutput(InputStream stream, Process process,
            AtomicBoolean oversized) throws IOException {
        ByteArrayOutputStream output = new ByteArrayOutputStream();
        byte[] chunk = new byte[8192];
        int length;
        while ((length = stream.read(chunk)) != -1) {
            if (length > MAX_WORKER_OUTPUT - output.size()) {
                oversized.set(true);
                process.destroyForcibly();
                return null;
            }
            output.write(chunk, 0, length);
        }
        return output.toByteArray();
    }

    static void runWorker(String[] args) {
        if (!(args.length == 2 && args[0].equals("--worker") && args[1].equals("inspect"))
                && !(args.length == 3 && args[0].equals("--worker") && args[1].equals("page")
                    && args[2].matches("[1-9][0-9]{0,8}"))
                && !(args.length == 3 && args[0].equals("--worker") && args[1].equals("office-inspect")
                    && officeMedia(args[2]))
                && !(args.length == 4 && args[0].equals("--worker") && args[1].equals("office-unit")
                    && args[2].matches("(0|[1-9][0-9]{0,4})") && officeMedia(args[3]))) {
            System.exit(3);
        }
        try {
            byte[] pdf = System.in.readNBytes(PdfDocumentParser.MAX_INPUT + 1);
            if (pdf.length > PdfDocumentParser.MAX_INPUT) {
                output("ERR\ninput_too_large");
            } else if (args[1].equals("office-inspect")) {
                output("OK\n" + OfficeDocumentParser.inspectJson(
                        OfficeDocumentParser.inspect(pdf, args[2])));
            } else if (args[1].equals("office-unit")) {
                output("OK\n" + OfficeDocumentParser.unitJson(
                        OfficeDocumentParser.parseUnit(pdf, args[3], Integer.parseInt(args[2]))));
            } else if (args[1].equals("inspect")) {
                output("OK\n" + PdfServer.inspectJson(PdfDocumentParser.inspect(pdf)));
            } else {
                int page = Integer.parseInt(args[2]);
                output("OK\n" + PdfServer.pageJson(PdfDocumentParser.parsePage(pdf, page)));
            }
            System.exit(0);
        } catch (PdfDocumentParser.ParseFailure ex) {
            output("ERR\n" + ex.code());
            System.exit(0);
        } catch (OfficeDocumentParser.Failure ex) {
            output("ERR\n" + ex.code());
            System.exit(0);
        } catch (Throwable ex) {
            // No document data, exception details or stack traces leave this worker.
            System.exit(3);
        }
    }

    private static boolean officeMedia(String value) {
        return OfficeDocumentParser.DOCX.equals(value) || OfficeDocumentParser.XLSX.equals(value);
    }

    private static void output(String text) {
        byte[] bytes = text.getBytes(StandardCharsets.UTF_8);
        System.out.write(bytes, 0, bytes.length);
        System.out.flush();
    }

    private static boolean isParserCode(String code) {
        return switch (code) {
            case "invalid_pdf", "encrypted_pdf", "page_out_of_range",
                    "page_limit_exceeded", "input_too_large", "output_too_large",
                    "invalid_docx", "invalid_xlsx", "encrypted_office",
                    "unit_limit", "unit_out_of_range", "unsupported_content" -> true;
            default -> false;
        };
    }

    private static String runtimeClasspath() {
        // In production the code source is the single shaded jar. Maven
        // Surefire starts from a bootstrap classpath containing test artifacts.
        var source = PdfServer.class.getProtectionDomain().getCodeSource().getLocation();
        if (source.getPath().endsWith(".jar")) {
            try {
                return Path.of(source.toURI()).toString();
            } catch (java.net.URISyntaxException ex) {
                throw new IllegalStateException("invalid parser artifact");
            }
        }
        return System.getProperty("surefire.test.class.path", System.getProperty("java.class.path"));
    }

    private static boolean isWindows() {
        return System.getProperty("os.name").startsWith("Windows");
    }

    private static void copyWindowsSystemVariable(java.util.Map<String, String> environment,
            String name) {
        String value = System.getenv(name);
        if (value != null) environment.put(name, value);
    }
}
