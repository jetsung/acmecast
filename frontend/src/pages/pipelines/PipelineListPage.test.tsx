/**
 * 流水线列表「删除」回归用例。
 *
 * 曾经的 bug：confirmDelete 在事件处理函数里调用 App.useApp()（React Hook），
 * 点击「删除」即抛 Invalid hook call，确认弹窗永远无法出现，流水线无法删除。
 * 修复要点：modal 实例在组件顶层解构，回调中只使用、不调用 hook。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { App, ConfigProvider } from "antd";
import zhCN from "antd/locale/zh_CN";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PipelineListPage } from "./PipelineListPage";

vi.mock("@/api/client", () => ({
  ApiError: class ApiError extends Error {},
  client: {
    GET: vi.fn(async (path: string) => {
      if (path === "/api/pipelines") {
        return {
          data: {
            data: {
              items: [
                {
                  id: 3,
                  name: "le-wildcard",
                  description: null,
                  enabled: true,
                  step_count: 2,
                  created_at: "2026-10-01T00:00:00Z",
                  updated_at: "2026-10-01T00:00:00Z",
                },
              ],
              total: 1,
            },
          },
          response: new Response(),
        };
      }
      throw new Error(`未预料的请求: ${path}`);
    }),
    POST: vi.fn(),
    PUT: vi.fn(),
    DELETE: vi.fn(),
  },
}));

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

function renderPage() {
  // modal.confirm 的门户是独立 React root（挂在 body 下），cleanup() 只卸载
  // testing-library 容器，管不到它——用标记区分页面容器，便于用例后清理。
  const container = document.createElement("div");
  container.dataset.pageContainer = "true";
  document.body.appendChild(container);
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      {/* main.tsx 生产挂载带 zhCN locale，弹窗默认按钮（取消）文案依赖它。 */}
      <ConfigProvider locale={zhCN}>
        <App>
          <MemoryRouter>
            <PipelineListPage />
          </MemoryRouter>
        </App>
      </ConfigProvider>
    </QueryClientProvider>,
    { container },
  );
}

describe("流水线删除确认", () => {
  beforeEach(async () => {
    vi.mocked((await import("@/api/client")).client.DELETE).mockReset();
  });

  afterEach(() => {
    cleanup();
    // 弹窗出场动画在 jsdom 中永不结束，DOM 不会自行卸载；不清掉会串进
    // 下一个用例（按钮查询会命中遗留弹窗）。
    for (const el of [...document.body.children]) {
      if (el instanceof HTMLElement && !el.dataset.pageContainer) {
        el.remove();
      }
    }
  });

  /** 在弹窗门户（.ant-modal）内按文字找按钮。antd 会在两字中文按钮里插空格（「删 除」）。 */
  function modalButton(pattern: RegExp): HTMLButtonElement | null {
    return (
      ([...document.querySelectorAll(".ant-modal .ant-btn")].find((el) =>
        pattern.test(el.textContent ?? ""),
      ) as HTMLButtonElement | undefined) ?? null
    );
  }

  async function openConfirmForRow() {
    renderPage();
    const deleteButtons = await screen.findAllByRole("button", { name: /删\s*除/ });
    fireEvent.click(deleteButtons[0]);
    // 确认弹窗渲染在 body 门户里，等它的 ok 按钮出现（okText 为「删除」）。
    return waitFor(() => {
      const ok = modalButton(/删\s*除/);
      expect(ok).toBeTruthy();
      return ok!;
    });
  }

  it("点击删除弹出确认框，未确认前不调删除接口", async () => {
    const { client } = await import("@/api/client");
    await openConfirmForRow();

    // 弹窗标题与影响提示（含流水线名）可见，页面未因 hook 报错崩溃。
    // 标题在 ant-modal-title 与 confirm-title 各渲染一份，断言出现即可。
    const titles = await screen.findAllByText("删除流水线？");
    expect(titles.length).toBeGreaterThan(0);
    expect(
      await screen.findByText("le-wildcard 及其步骤、调度将一并删除。"),
    ).toBeTruthy();
    expect(client.DELETE).not.toHaveBeenCalled();
  });

  it("取消确认不调用删除接口", async () => {
    const { client } = await import("@/api/client");
    await openConfirmForRow();

    const cancel = modalButton(/取\s*消/);
    expect(cancel).toBeTruthy();
    fireEvent.click(cancel!);

    // 留出调用窗口：若误触发删除，这段等待内必然发出请求。
    await sleep(100);
    expect(client.DELETE).not.toHaveBeenCalled();
    // 列表保持原状。
    expect(await screen.findByText("le-wildcard")).toBeTruthy();
  });

  it("确认后调用删除接口并刷新列表", async () => {
    const { client } = await import("@/api/client");
    vi.mocked(client.DELETE).mockResolvedValueOnce({
      data: { data: null },
      response: new Response(),
    } as never);
    await openConfirmForRow();

    const ok = modalButton(/删\s*除/);
    fireEvent.click(ok!);

    await waitFor(() => {
      expect(client.DELETE).toHaveBeenCalledWith("/api/pipelines/3", {
        params: { path: { id: 3 } },
      });
    });
    // 成功提示 + invalidateQueries 触发列表重新请求（初始加载 1 次 + 刷新 1 次）。
    expect(await screen.findByText("已删除")).toBeTruthy();
    await waitFor(() => {
      expect(client.GET).toHaveBeenCalledTimes(2);
    });
  });

  it("删除失败时提示错误且列表保留", async () => {
    const { client } = await import("@/api/client");
    vi.mocked(client.DELETE).mockResolvedValueOnce({
      error: { error: { code: "internal", message: "boom" } },
      response: new Response(null, { status: 500 }),
    } as never);
    await openConfirmForRow();

    const ok = modalButton(/删\s*除/);
    fireEvent.click(ok!);

    expect(await screen.findByText("删除失败")).toBeTruthy();
    expect(await screen.findByText("le-wildcard")).toBeTruthy();
  });
});
