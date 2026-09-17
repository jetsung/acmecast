import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import path from "node:path";

// 开发期把 /api 代理到本地后端（D10）：同源路径，生产同源部署同样无 CORS。
export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "src"),
    },
  },
  server: {
    proxy: {
      "/api": {
        target: "http://127.0.0.1:8080",
        changeOrigin: false,
      },
    },
  },
});
