import { defineConfig, devices } from "@playwright/test";

/**
 * Playwright e2e：用真实的 Chromium 验证用量化单测覆盖不了的东西
 * （典型是布局——jsdom 没有布局引擎，autoSize 行高测不出来）。
 *
 * 后端不参与：测试用 page.route 拦截 /api，喂固定数据，所以只需前端 dev server。
 */
export default defineConfig({
  testDir: "./e2e",
  fullyParallel: true,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 1 : 0,
  reporter: "list",
  use: {
    // 用 localhost：Vite 默认只绑 localhost（本环境解析到 ::1），127.0.0.1 连不上。
    baseURL: "http://localhost:4173",
    trace: "on-first-retry",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: {
    // 用构建产物 + preview，而不是 dev server：dev 首次访问懒加载路由会触发
    // 依赖预构建并整页 reload，动态 import 会偶发失败（Failed to fetch dynamically
    // imported module）。preview 没有这套机制，也更接近生产。
    command: "pnpm build && pnpm preview --port 4173 --strictPort",
    url: "http://localhost:4173",
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
});
