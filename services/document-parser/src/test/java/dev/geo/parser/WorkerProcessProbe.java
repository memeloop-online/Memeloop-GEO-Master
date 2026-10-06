package dev.geo.parser;

/** Fixed-purpose test child only; not packaged in the shaded production jar. */
public final class WorkerProcessProbe {
    private WorkerProcessProbe() {}

    public static void main(String[] args) throws Exception {
        switch (args[0]) {
            case "hang" -> Thread.sleep(60_000);
            case "exit" -> System.exit(7);
            case "env" -> {
                if (System.getenv().size() > (System.getProperty("os.name").startsWith("Windows")
                        ? 2 : 0)
                        || System.getenv().containsKey("JAVA_TOOL_OPTIONS")) {
                    System.exit(7);
                }
                System.out.print("OK\n{}");
            }
            case "oversize" -> {
                byte[] chunk = new byte[8192];
                for (int i = 0; i < 5_000; i++) System.out.write(chunk);
                System.out.flush();
            }
            default -> System.exit(7);
        }
    }
}
