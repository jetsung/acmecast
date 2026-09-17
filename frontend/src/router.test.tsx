/**
 * 路由懒加载：页面模块只在对应路由被渲染时才 import。
 *
 * 用「记录模块何时被 import」的 mock 替换两个页面：真实页面会连带渲染大量
 * antd 组件并请求接口，而这个用例要验证的是**路由的拆分行为**，不是页面本身。
 */
import { render, screen } from "@testing-library/react";
import { RouterProvider, createMemoryRouter } from "react-router";
import { beforeEach, describe, expect, it, vi } from "vitest";

// vi.mock 会被提升到文件顶部，工厂函数则在模块**真正被 import 时**才执行——
// 这正是懒加载的判据：路由没渲染到，页面模块就不该被 import。
const importedPages = vi.hoisted(() => new Set<string>());

vi.mock("@/pages/DashboardPage", () => {
  importedPages.add("DashboardPage");
  return { DashboardPage: () => <div>dashboard-page</div> };
});

vi.mock("@/pages/credentials/CredentialListPage", () => {
  importedPages.add("CredentialListPage");
  return { CredentialListPage: () => <div>credentials-page</div> };
});

import { App as AntApp } from "antd";
import { useAuthStore } from "@/auth/store";
import { routes } from "@/router";

/** 在内存路由上渲染某个初始路径，返回 router 供测试内导航。 */
function renderAt(initialPath: string) {
  const router = createMemoryRouter(routes, { initialEntries: [initialPath] });
  render(
    <AntApp>
      <RouterProvider router={router} />
    </AntApp>,
  );
  return router;
}

beforeEach(() => {
  // 受保护路由只看令牌是否存在，不做校验，给个值即可通过。
  useAuthStore.getState().login("test-token", "admin");
});

describe("路由懒加载", () => {
  it("只 import 被渲染到的路由页面", async () => {
    // 路由表已经构建（lazy 只是登记了加载函数），页面模块一个都还没加载。
    expect(importedPages.size).toBe(0);

    const router = renderAt("/");
    expect(await screen.findByText("dashboard-page")).toBeTruthy();
    expect([...importedPages]).toEqual(["DashboardPage"]);

    // 导航到另一条路由，此时才加载该页面的模块。
    await router.navigate("/credentials");
    expect(await screen.findByText("credentials-page")).toBeTruthy();
    expect(importedPages.has("CredentialListPage")).toBe(true);
    // 已加载的模块不会重复加载。
    expect(importedPages.size).toBe(2);
  });

  it("真实页面同样经动态 import 解析后才渲染", async () => {
    renderAt("/login");

    // LoginPage 是真实模块（未 mock）：必须等它的 chunk 解析出来才看得到表单。
    // antd 会在两个汉字之间自动插空格（"登 录"），所以用正则匹配。
    expect(await screen.findByRole("button", { name: /登\s*录/ })).toBeTruthy();
    expect(screen.getByText("acmecast 控制台")).toBeTruthy();
  });
});
