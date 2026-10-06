import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import { loadEnv } from "vite";
import { createHash } from "node:crypto";
import { readFileSync, readdirSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), "");
  const novncRoot = dirname(
    dirname(createRequire(import.meta.url).resolve("@novnc/novnc")),
  );
  return {
    cacheDir:
      env.GEO_VITE_CACHE_DIR ??
      join(
        tmpdir(),
        "memeloop-geo-vite",
        createHash("sha256").update(process.cwd()).digest("hex").slice(0, 16),
      ),
    plugins: [
      react(),
      {
        name: "retain-novnc-license-notices",
        generateBundle() {
          for (const name of readdirSync(join(novncRoot, "docs")).filter(
            (file) => file.startsWith("LICENSE."),
          )) {
            this.emitFile({
              type: "asset",
              fileName: `third-party/noVNC/${name}`,
              source: readFileSync(join(novncRoot, "docs", name)),
            });
          }
          for (const [name, path] of [
            ["AUTHORS", join(novncRoot, "AUTHORS")],
            ["pako-LICENSE", join(novncRoot, "vendor", "pako", "LICENSE")],
            [
              "THIRD_PARTY_NOTICES.md",
              join(process.cwd(), "THIRD_PARTY_NOTICES.md"),
            ],
          ]) {
            this.emitFile({
              type: "asset",
              fileName: `third-party/noVNC/${name}`,
              source: readFileSync(path),
            });
          }
        },
      },
    ],
    server: {
      proxy: {
        "/api": {
          target: env.VITE_DEV_API_PROXY_TARGET ?? "http://127.0.0.1:8080",
          // Keep the browser Host so the API's Host -> Operator mapping remains
          // authoritative in development as it is in production.
          changeOrigin: false,
          ws: true,
        },
      },
    },
    optimizeDeps: {
      esbuildOptions: { target: "es2022" },
    },
    build: {
      // noVNC 1.7 uses top-level await for its WebCodecs capability probe.
      target: "es2022",
      rollupOptions: {
        output: {
          manualChunks(id) {
            if (id.includes("@fluentui")) return "fluent";
            if (id.includes("@tanstack")) return "query";
            if (id.includes("react-router")) return "router";
            if (
              id.includes("/node_modules/react") ||
              id.includes("/node_modules/scheduler")
            ) {
              return "react";
            }
          },
        },
      },
    },
    test: {
      // Keep heavyweight jsdom/font suites from starving one another on CI.
      // This affects test workers only, not application or agent concurrency.
      maxWorkers: process.env.CI ? 2 : undefined,
      environment: "jsdom",
      globals: true,
      setupFiles: "./src/test/setup.ts",
      css: true,
    },
  };
});
