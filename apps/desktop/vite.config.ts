import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// Tauri expects a fixed dev port and no interactive output rewriting.
//
// 这里曾经还挂着一个 `tailwindcss()` 插件。它是纯粹的负担：
// src/styles.css 里没有 `@import "tailwindcss"`、也没有 `@tailwind` 指令，
// 没有任何组件写过工具类 —— 这套界面的样式全部是手写的语义类名加自定义属性。
// 插件因此对每一份 CSS 和 TSX 都跑一遍转换，产出为零。
// 移除后构建产物逐字节相同（SHA-256 一致），可以确认它确实什么都没做。
export default defineConfig({
  plugins: [react()],
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
