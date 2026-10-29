import { defineConfig } from "vite";

// UI живёт в ui/, сборка кладётся в dist/ — оттуда её забирает Tauri (app/tauri.conf.json).
export default defineConfig({
  root: "ui",
  clearScreen: false,
  server: {
    // Явно IPv4: иначе localhost в Node слушается только как ::1, а Tauri ждёт 127.0.0.1.
    host: "127.0.0.1",
    port: 5173,
    strictPort: true,
  },
  build: {
    outDir: "../dist",
    emptyOutDir: true,
    target: ["es2022", "chrome110", "safari16"],
  },
});
