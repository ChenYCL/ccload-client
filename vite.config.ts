import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import path from "node:path";
import { readFileSync } from "node:fs";

// 客户端自己的版本号。侧栏底部那行显示的是**内核**版本（运行时从内核问来的），
// 跟这个是两回事 —— 之前壳体版本在界面上根本没出现过，用户装了哪个 beta 只能去
// 「关于」或者文件名里翻。发版时改的就是 package.json 这一处，直接读它，不另设常量：
// 手写的常量必然漂移（内核版本那块就曾经停在 "v1.2.0" 而实际打进去的是 v4.6.x）。
const clientVersion = JSON.parse(
  readFileSync(path.resolve(__dirname, "package.json"), "utf8"),
).version;

// Tauri serves the renderer from a fixed port and expects a plain SPA build.
export default defineConfig({
  plugins: [react()],
  root: "src",
  publicDir: false,
  define: {
    __CLIENT_VERSION__: JSON.stringify(clientVersion),
  },
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "src"),
      // 应用图标就是产品 logo，侧栏要用它。走 alias 指向打包用的那一份，而不是
      // 往 src/ 下复制一张 —— 复制出来的那张改图标时没人记得同步。
      "@icons": path.resolve(__dirname, "src-tauri/icons"),
    },
  },
  server: {
    port: 5273,
    strictPort: true,
    host: "127.0.0.1",
    // root 是 src/，图标在它外面；dev server 默认不放行 root 之外的文件。
    fs: { allow: [path.resolve(__dirname)] },
  },
  build: {
    outDir: "../dist",
    emptyOutDir: true,
    target: "safari15",
    sourcemap: false,
  },
});
