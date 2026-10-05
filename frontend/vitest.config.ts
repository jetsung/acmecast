import { defineConfig, mergeConfig } from "vitest/config";
import viteConfig from "./vite.config";

// 复用 vite.config.ts（@ 别名、react 插件、manualChunks），只补测试运行环境。
export default mergeConfig(
  viteConfig,
  defineConfig({
    test: {
      environment: "jsdom",
      setupFiles: ["./src/test/setup.ts"],
      include: ["src/**/*.test.{ts,tsx}"],
    },
  }),
);
