package dev.geo.parser;

import static org.junit.jupiter.api.Assertions.*;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Path;
import java.util.zip.ZipFile;
import org.junit.jupiter.api.Test;

/** Verify licenses in the distributable, not just in Maven's source dependencies. */
class ShadedLicenseIT {
    @Test
    void shadedJarIncludesDistinctLicensesAndNotices() throws Exception {
        try (ZipFile jar = new ZipFile(Path.of(
                System.getProperty("shadedJarPath")).toFile())) {
            assertContains(jar, "META-INF/LICENSE", "Apache License");
            assertContains(jar, "META-INF/LICENSE.txt", "QOS.ch Sarl");
            assertContains(jar, "META-INF/LICENSE.txt", "Apache License");
            assertContains(jar, "META-INF/LICENSE.md", "Copyright (c) 2017, 2018 Oracle");
            assertContains(jar, "META-INF/LICENSE.md", "Copyright (c) 2018 Oracle");
            assertContains(jar, "META-INF/LICENSES/Bouncy-Castle.txt",
                    "The Legion of the Bouncy Castle");
            assertContains(jar, "META-INF/NOTICE", "Apache Tika");
            assertContains(jar, "META-INF/NOTICE", "Apache POI");
            assertContains(jar, "META-INF/NOTICE", "Apache Log4j API");
            assertContains(jar, "META-INF/NOTICE", "BEA Systems");
            assertContains(jar, "META-INF/LICENSE", "Apache License");
            assertContains(jar, "META-INF/NOTICE.md", "Eclipse Angus");
            assertContains(jar, "META-INF/NOTICE.txt", "Apache Commons IO");
        }
    }

    private static void assertContains(ZipFile jar, String filename, String expected)
            throws IOException {
        var entry = jar.getEntry(filename);
        assertNotNull(entry, filename);
        try (var content = jar.getInputStream(entry)) {
            assertTrue(new String(content.readAllBytes(), StandardCharsets.UTF_8)
                    .contains(expected), filename + " lacks expected license/notice text");
        }
    }
}
