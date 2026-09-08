import { defineConfig } from "vite";
import { fileURLToPath } from "node:url";

export default defineConfig({
  define: { "process.env.NODE_ENV": JSON.stringify("production") },
  build: {
    outDir: "../codex-rs/report/assets",
    emptyOutDir: false,
    lib: {
      entry: fileURLToPath(new URL("./src/main.tsx", import.meta.url)),
      name: "BetterCodexReport",
      formats: ["iife"],
      fileName: () => "dashboard.js",
      cssFileName: "dashboard",
    },
    rollupOptions: { output: { inlineDynamicImports: true } },
  },
});
