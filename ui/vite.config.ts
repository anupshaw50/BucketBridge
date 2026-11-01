import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// The Tauri host (crates/desktop) loads http://localhost:5173 in development
// and ../../ui/dist in release builds, so the port is fixed.
const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  envPrefix: ["VITE_", "TAURI_ENV_"],
  server: {
    port: 5173,
    strictPort: true,
    host: host ?? false,
    hmr: host ? { protocol: "ws", host, port: 5174 } : undefined,
    watch: { ignored: ["**/crates/**", "**/target/**"] },
  },
  preview: {
    port: 4173,
    strictPort: true,
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: Boolean(process.env.TAURI_ENV_DEBUG),
    minify: !process.env.TAURI_ENV_DEBUG,
    chunkSizeWarningLimit: 1500,
  },
});
