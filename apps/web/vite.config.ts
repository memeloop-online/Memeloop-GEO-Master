import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
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
});
