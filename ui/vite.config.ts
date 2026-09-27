import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { createRequire } from "node:module";

const resolveDependency = createRequire(import.meta.url);

// The window talks to the engine over Tauri IPC only; there is no HTTP API to
// proxy. `tauri dev` loads this server on port 5174.
export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      // The browser decoder uses document.createElement. Its equivalent
      // DOM-free export is required inside the Markdown worker as well.
      "decode-named-character-reference": resolveDependency.resolve(
        "decode-named-character-reference",
      ),
    },
  },
  server: {
    port: 5174,
    strictPort: true,
  },
  build: {
    outDir: "dist",
    rollupOptions: {
      output: { manualChunks: { markdown: ["react-markdown", "remark-gfm"] } },
    },
  },
});
