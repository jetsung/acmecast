/**
 * 证书列表「到期时间」列的可读性回归用例。
 *
 * 曾经的问题：列宽 200px 装不下「剩余天数 Tag + zh-CN 完整时间」，
 * 日期被折成两行。修复为 260px；本用例钉住列宽与单元格内容同格渲染。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { App } from "antd";
import { MemoryRouter } from "react-router";
import { describe, expect, it, vi } from "vitest";
import { client } from "@/api/client";
import { CertificateListPage } from "./CertificateListPage";

const mockedGet = vi.mocked(client.GET);

/** 从某次 client.GET 调用中取出列表查询参数。 */
function queryOf(callIndex: number): Record<string, unknown> {
  const init = mockedGet.mock.calls[callIndex]?.[1] as
    | { params: { query: Record<string, unknown> } }
    | undefined;
  if (!init) throw new Error(`client.GET 第 ${callIndex} 次调用不存在`);
  return init.params.query;
}

vi.mock("@/api/client", () => ({
  ApiError: class ApiError extends Error {},
  client: {
    GET: vi.fn(async (path: string, _init?: unknown) => {
      if (path === "/api/certificates") {
        const notAfter = new Date(Date.now() + 90 * 24 * 3600 * 1000).toISOString();
        return {
          data: {
            data: {
              items: [
                {
                  id: 1,
                  domains: ["example.com"],
                  fingerprint: "fp-1",
                  not_after: notAfter,
                  revoked_at: null,
                  updated_at: "2026-09-19T00:00:00Z",
                },
              ],
              total: 1,
              page: 1,
              page_size: 20,
            },
          },
          response: new Response(),
        };
      }
      throw new Error(`未预料的请求: ${path}`);
    }),
  },
}));

function renderPage() {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <App>
        <MemoryRouter>
          <CertificateListPage />
        </MemoryRouter>
      </App>
    </QueryClientProvider>,
  );
}

describe("证书列表到期时间列", () => {
  it("到期时间列宽 260px，天数标记与时间同在到期时间单元格内", async () => {
    const { container } = renderPage();

    // 列宽钉在 260：antd 会把列宽写到 colgroup 的 col 元素上。
    await waitFor(() => {
      const cols = [...container.querySelectorAll("colgroup col")];
      expect(
        cols.some((col) => (col as HTMLElement).style.width === "260px"),
        "到期时间列应渲染为 260px 宽",
      ).toBe(true);
    });

    // 天数标记与本地化时间同在一个单元格，共享同一行。
    // 不钉具体天数：mock 生成 not_after 后渲染时刻的时钟仍在走，
    // floor 会把天数往下取整（90 → 89），钉死数字会造成偶发失败。
    await waitFor(() => {
      const tag = container.querySelector("td .ant-tag");
      expect(tag?.textContent, "到期单元格应先渲染剩余天数标记").toMatch(/^\d+ 天$/);
      const cell = tag?.closest("td");
      expect(cell?.textContent).toMatch(/\d{4}\/\d{1,2}\/\d{1,2}/);
    });
  });
});

describe("证书列表默认排序", () => {
  it("首次渲染即按到期时间降序请求（sort=descending, page=1）", async () => {
    renderPage();

    await waitFor(() => {
      expect(mockedGet).toHaveBeenCalled();
    });
    expect(queryOf(0)).toMatchObject({ sort: "descending", page: 1 });
  });

  it("切换为「按到期时间升序」后重新按升序请求", async () => {
    const { container } = renderPage();

    await waitFor(() => {
      expect(mockedGet).toHaveBeenCalled();
    });

    // 打开 antd Select 下拉（选项挂在 body 的 portal 中），点选升序。
    // antd v6 的 Select 已无 .ant-select-selector 包装层，
    // 对根元素派发 mousedown，事件冒泡到 rc-select 的处理器。
    fireEvent.mouseDown(container.querySelector(".ant-select")!);
    fireEvent.click(await screen.findByText("按到期时间升序"));

    await waitFor(() => {
      const last = mockedGet.mock.calls.length - 1;
      expect(queryOf(last)).toMatchObject({ sort: "ascending", page: 1 });
    });
  });
});
