/**
 * 登出流程的端到端验证。
 *
 * jsdom 单测钉住了 store 状态与跳转调用；这里在真实浏览器里验证
 * 用户可见的完整链路：点击登出 → 登录态被清除并落在登录页 →
 * 本地无令牌时访问受保护路由被守卫拦回登录页（与登出后的状态一致）。
 *
 * 后端不参与：/api 全部桩掉（登出本身不调后端——令牌无状态）。
 * 注意 addInitScript 对后续每次导航都会重新执行，会复活登录态，
 * 所以「受保护路由拦截」用例不预置令牌，直接验证无令牌守卫。
 */
import { expect, test } from "@playwright/test";

test.beforeEach(async ({ page }) => {
  await page.route("**/api/**", (route) =>
    route.fulfill({ status: 404, json: { error: { code: "not_found", message: "e2e stub" } } }),
  );
});

test("点击登出后回到登录页并清除本地登录态", async ({ page }) => {
  // 预置登录态：路由守卫只看令牌是否存在，塞一个即可通过。
  await page.addInitScript(() => {
    localStorage.setItem(
      "acmecast-auth",
      JSON.stringify({ state: { token: "e2e-token", username: "admin" }, version: 0 }),
    );
  });
  await page.goto("/");

  // 侧边栏底部的登出入口。
  const logout = page.getByRole("button", { name: /登\s*出/ });
  await expect(logout).toBeVisible();
  await logout.click();

  // React Router 的跳转是同文档导航，用轮询断言而不是等待 load 事件。
  // RequireAuth 重定向会带上 ?next=<原路径> 查询参数。
  await expect(page).toHaveURL(/\/login(\?|$)/);
  await expect(page.getByText("acmecast 控制台")).toBeVisible();

  const auth = await page.evaluate(() =>
    JSON.parse(localStorage.getItem("acmecast-auth") ?? "{}"),
  );
  expect(auth?.state?.token ?? null).toBeNull();
});

test("本地无令牌时访问受保护路由会被重定向回登录页", async ({ page }) => {
  // 与「点击登出后」的本地状态一致：无令牌。守卫应拦下并跳登录页。
  await page.goto("/certificates");
  await expect(page).toHaveURL(/\/login(\?|$)/);
});
