import { defineConfig } from "vite";

// UI живёт в ui/, сборка кладётся в dist/ — оттуда её забирает Tauri (app/tauri.conf.json).
export default defineConfig({
  root: "ui",
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    outDir: "../dist",
    emptyOutDir: true,
    target: ["es2022", "chrome110", "safari16"],
  },
});
