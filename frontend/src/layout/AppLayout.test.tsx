/**
 * 侧边栏登出入口。
 *
 * 背景：登录态此前只能等令牌过期后由 401 被动清除，没有主动登出入口。
 * 登出按钮复用 401 自动登出的同一 `logout()` action：清除本地持久化的
 * 令牌与用户名后跳转登录页（design D1，console-auth spec）。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen } from "@testing-library/react";
import { App } from "antd";
import { MemoryRouter, Route, Routes } from "react-router";
import { beforeEach, describe, expect, it } from "vitest";
import { useAuthStore } from "@/auth/store";
import { AppLayout } from "./AppLayout";

function renderLayout() {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <App>
        <MemoryRouter initialEntries={["/"]}>
          <Routes>
            <Route path="/" element={<AppLayout />} />
            <Route path="/login" element={<div>登录页</div>} />
          </Routes>
        </MemoryRouter>
      </App>
    </QueryClientProvider>,
  );
}

describe("侧边栏登出", () => {
  beforeEach(() => {
    useAuthStore.setState({ token: "test-token", username: "admin" });
  });

  it("点击登出后清除本地登录态并跳转登录页", async () => {
    renderLayout();
    expect(useAuthStore.getState().token).toBe("test-token");

    // antd 会在两字中文按钮里插空格（「登 出」），用正则匹配。
    fireEvent.click(screen.getByRole("button", { name: /登\s*出/ }));

    await screen.findByText("登录页");
    expect(useAuthStore.getState().token).toBeNull();
    expect(useAuthStore.getState().username).toBeNull();
  });
});
