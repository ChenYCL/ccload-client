import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import path from "node:path";

// Tauri serves the renderer from a fixed port and expects a plain SPA build.
export default defineConfig({
  plugins: [react()],
  root: "src",
  publicDir: false,
  resolve: {
    alias: { "@": path.resolve(__dirname, "src") },
  },
  server: {
    port: 5273,
    strictPort: true,
    host: "127.0.0.1",
  },
  build: {
    outDir: "../dist",
    emptyOutDir: true,
    target: "safari15",
    sourcemap: false,
  },
});
