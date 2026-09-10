import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";

// Tauri expects a fixed dev port and no interactive output rewriting.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  server: {
    host: "127.0.0.1",
    port: 1420,
    strictPort: true,
    // `**/src-tauri/**` is Rust source, not frontend.
    // The `.tmpdir` pattern covers editors that save atomically (write a temp
    // file, then rename). Vite tries to watch that temp file while it is still
    // locked and dies with EBUSY on Windows, taking the dev server down.
    watch: { ignored: ["**/src-tauri/**", "**/*.tmpdir/**"] },
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: "es2022",
    outDir: "dist",
    sourcemap: true,
  },
});
