import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import { loadEnv } from "vite";

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), "");
  return {
    plugins: [react()],
    server: {
      proxy: {
        "/api": {
          target: env.VITE_DEV_API_PROXY_TARGET ?? "http://127.0.0.1:8080",
          // Keep the browser Host so the API's Host -> Operator mapping remains
          // authoritative in development as it is in production.
          changeOrigin: false,
        },
      },
    },
    build: {
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
      environment: "jsdom",
      globals: true,
      setupFiles: "./src/test/setup.ts",
      css: true,
    },
  };
});
