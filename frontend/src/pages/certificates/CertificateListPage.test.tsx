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
import { beforeEach, describe, expect, it, vi } from "vitest";
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
                  key_algorithm: "ECDSA P-256",
                  description: null,
                  not_after: notAfter,
                  revoked_at: null,
                  updated_at: "2026-09-19T00:00:00Z",
                },
                {
                  id: 2,
                  domains: ["legacy.example.com"],
                  fingerprint: "fp-2",
                  key_algorithm: null,
                  description: null,
                  not_after: notAfter,
                  revoked_at: null,
                  updated_at: "2026-09-19T00:00:00Z",
                },
              ],
              total: 2,
              page: 1,
              page_size: 15,
            },
          },
          response: new Response(),
        };
      }
      throw new Error(`未预料的请求: ${path}`);
    }),
    PATCH: vi.fn(),
    POST: vi.fn(),
    DELETE: vi.fn(),
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

describe("证书列表描述列", () => {
  it("表头包含描述列，无描述时显示占位", async () => {
    const { container } = renderPage();
    await waitFor(() => expect(mockedGet).toHaveBeenCalled());
    expect(container.textContent).toContain("描述");
    await waitFor(() => expect(container.textContent).toContain("—"));
  });
});

describe("证书列表 ID 列", () => {
  it("首列为 ID 列（宽 80px）且渲染行的数字 ID", async () => {
    const { container } = renderPage();
    await waitFor(() => expect(mockedGet).toHaveBeenCalled());

    expect(container.textContent).toContain("ID");
    // 列宽钉在 80：antd 会把列宽写到 colgroup 的 col 元素上。
    await waitFor(() => {
      const cols = [...container.querySelectorAll("colgroup col")];
      expect(
        cols.some((col) => (col as HTMLElement).style.width === "80px"),
        "ID 列应渲染为 80px 宽",
      ).toBe(true);
    });
    // 行内渲染数字 ID（与 mock 数据 id:1 对应）。
    await waitFor(() => {
      const cell = [...container.querySelectorAll("td")].find(
        (td) => td.textContent === "1",
      );
      expect(cell, "应有单元格渲染数字 ID").toBeTruthy();
    });
  });
});

describe("证书列表算法列", () => {
  it("展示私钥算法，缺失算法的存量记录显示占位", async () => {
    const { container } = renderPage();
    await waitFor(() => expect(mockedGet).toHaveBeenCalled());

    expect(container.textContent).toContain("算法");
    expect(await screen.findByText("ECDSA P-256")).toBeTruthy();
    await waitFor(() => {
      expect(container.textContent).toContain("—");
    });
  });
});

describe("证书描述内联编辑", () => {
  it("点击编辑弹出输入框，保存调用 PATCH 并刷新列表", async () => {
    const { client } = await import("@/api/client");
    vi.mocked(client.PATCH).mockResolvedValueOnce({
      data: { data: { id: 1, description: "生产环境主域" } },
      response: new Response(),
    } as never);

    const { container } = renderPage();
    const editButtons = await screen.findAllByRole("button", {
      name: /编辑证书 \d+ 的备注/,
    });
    fireEvent.click(editButtons[0]);

    // Popover 打开后输入备注并保存（回车或点击保存按钮）。
    const input = await screen.findByLabelText("证书 1 备注输入");
    fireEvent.change(input, { target: { value: "  生产环境主域  " } });
    fireEvent.click(screen.getByRole("button", { name: /保\s*存/ }));

    await waitFor(() => {
      expect(client.PATCH).toHaveBeenCalledWith("/api/certificates/1", {
        params: { path: { id: 1 } },
        body: { description: "  生产环境主域  " },
      });
    });
    // 保存成功后列表重新请求（初始 1 次 + 刷新 1 次）。
    await waitFor(() => {
      expect(mockedGet.mock.calls.length).toBeGreaterThanOrEqual(2);
    });
    expect(container).toBeTruthy();
  }, 20000);

  it("保存失败时展示后端错误消息", async () => {
    const { client } = await import("@/api/client");
    vi.mocked(client.PATCH).mockResolvedValueOnce({
      error: { error: { code: "internal", message: "保存备注失败" } },
      response: new Response(null, { status: 500 }),
    } as never);

    renderPage();
    const editButtons = await screen.findAllByRole("button", {
      name: /编辑证书 \d+ 的备注/,
    });
    fireEvent.click(editButtons[0]);

    const input = await screen.findByLabelText("证书 1 备注输入");
    fireEvent.change(input, { target: { value: "x" } });
    fireEvent.click(screen.getByRole("button", { name: /保\s*存/ }));

    expect(await screen.findByText("保存备注失败")).toBeTruthy();
  }, 20000);
});


describe("证书列表吊销与归档列", () => {
  beforeEach(() => vi.clearAllMocks());
  // 隔离之前的 mock 状态
  it("操作列内同时提供吊销与归档按钮", async () => {
    const { container } = renderPage();
    await waitFor(() => expect(mockedGet).toHaveBeenCalled());
    await waitFor(() => expect(container.textContent).toContain("操作"));
    // 操作列内每行各有一个吊销与归档按钮
    expect(await screen.findAllByRole("button", { name: /吊销证书 \d+/ })).toHaveLength(2);
    expect(await screen.findAllByRole("button", { name: /归档证书 \d+/ })).toHaveLength(2);
    // 不再有独立的“吊销”“归档”列头
    const headers = [...container.querySelectorAll("th")].map((th) => th.textContent?.trim());
    expect(headers.filter((h) => h === "吊销")).toHaveLength(0);
    expect(headers.filter((h) => h === "归档")).toHaveLength(0);
    expect(headers).toContain("操作");
  }, 20000);

  it("点击吊销二次确认后调用 POST 并刷新列表", async () => {
    const { client } = await import("@/api/client");
    const post = vi.mocked(client.POST);
    post.mockResolvedValueOnce({
      data: { data: { cert_id: 1, revoked_now: true, revoked_at: new Date().toISOString() } },
      response: new Response(),
    } as never);
    renderPage();
    const btn = (await screen.findAllByRole("button", { name: /吊销证书 1/ }))[0];
    fireEvent.click(btn);
    await screen.findAllByText("确认吊销证书？");
    const confirms = await screen.findAllByRole("button", { name: /吊\s*销/ });
    // modal 中的确认按钮是最后一个匹配项
    fireEvent.click(confirms[confirms.length - 1]);
    await waitFor(() => {
      expect(post).toHaveBeenCalledWith(
        "/api/certificates/1/revoke",
        expect.objectContaining({ params: { path: { id: 1 } } }),
      );
    });
    await waitFor(() => {
      expect(mockedGet.mock.calls.length).toBeGreaterThanOrEqual(2);
    });
  }, 20000);


  it("已吊销时归档与吊销均禁用，已归档时仍可吊销", async () => {
    const { client } = await import("@/api/client");
    // 让第一条记录为已吊销
    vi.mocked(client.GET).mockResolvedValueOnce({
      data: {
        data: {
          items: [
            {
              id: 1,
              domains: ["revoked.example.com"],
              fingerprint: "fp-r",
              key_algorithm: "ECDSA P-256",
              description: null,
              not_after: new Date(Date.now() + 90 * 86400000).toISOString(),
              revoked_at: new Date().toISOString(),
              updated_at: "2026-09-19T00:00:00Z",
            },
            {
              id: 2,
              domains: ["archived.example.com"],
              fingerprint: "fp-a",
              key_algorithm: "ECDSA P-256",
              description: null,
              not_after: new Date(Date.now() + 90 * 86400000).toISOString(),
              revoked_at: null,
              updated_at: "2026-09-19T00:00:00Z",
            },
          ],
          total: 2,
          page: 1,
          page_size: 15,
        },
      },
      response: new Response(),
    } as never);
    renderPage();
    const revokeBtns = await screen.findAllByRole("button", { name: /吊销证书 \d+/ });
    const archiveBtns = await screen.findAllByRole("button", { name: /归档证书 \d+/ });
    // 第1行已吊销：两个按钮均禁用
    expect(revokeBtns[0].hasAttribute("disabled") || revokeBtns[0].getAttribute("aria-disabled") === "true" || (revokeBtns[0] as HTMLButtonElement).disabled).toBe(true);
    expect(archiveBtns[0].hasAttribute("disabled") || archiveBtns[0].getAttribute("aria-disabled") === "true" || (archiveBtns[0] as HTMLButtonElement).disabled).toBe(true);
    // 第2行未吊销：两个按钮均可用（吊销与归档不因归档状态互斥）
    expect((revokeBtns[1] as HTMLButtonElement).disabled).toBe(false);
    expect((archiveBtns[1] as HTMLButtonElement).disabled).toBe(false);
  }, 20000);

  it("点击归档二次确认后调用 POST 并刷新列表", async () => {
    const { client } = await import("@/api/client");
    const post = vi.mocked(client.POST);
    post.mockResolvedValueOnce({
      data: { data: { cert_id: 1, cert_pem_path: "certs/archived/fp-1/a.cert.pem", key_pem_path: "certs/archived/fp-1/a.key.pem", archived_now: true } },
      response: new Response(),
    } as never);
    renderPage();
    const btn = (await screen.findAllByRole("button", { name: /归档证书 1/ }))[0];
    fireEvent.click(btn);
    await screen.findAllByText("确认归档证书？");
    const confirms = await screen.findAllByRole("button", { name: /归\s*档/ });
    fireEvent.click(confirms[confirms.length - 1]);
    await waitFor(() => {
      expect(post).toHaveBeenCalledWith(
        "/api/certificates/1/archive",
        expect.objectContaining({ params: { path: { id: 1 } } }),
      );
    });
    await waitFor(() => {
      expect(mockedGet.mock.calls.length).toBeGreaterThanOrEqual(2);
    });
  }, 20000);

  it("活跃态删除禁用，归档/吊销态删除启用", async () => {
    vi.mocked(client.GET).mockResolvedValueOnce({
      data: {
        data: {
          items: [
            { id: 10, domains: ["a.example.com"], fingerprint: "fp-10", key_algorithm: "ECDSA P-256", description: null, not_after: new Date(Date.now() + 90 * 86400000).toISOString(), revoked_at: null, archived_at: null, updated_at: "2026-09-19T00:00:00Z" },
            { id: 11, domains: ["b.example.com"], fingerprint: "fp-11", key_algorithm: "ECDSA P-256", description: null, not_after: new Date(Date.now() + 90 * 86400000).toISOString(), revoked_at: null, archived_at: new Date().toISOString(), updated_at: "2026-09-19T00:00:00Z" },
            { id: 12, domains: ["c.example.com"], fingerprint: "fp-12", key_algorithm: "ECDSA P-256", description: null, not_after: new Date(Date.now() + 90 * 86400000).toISOString(), revoked_at: new Date().toISOString(), archived_at: null, updated_at: "2026-09-19T00:00:00Z" },
          ],
          total: 3,
          page: 1,
          page_size: 15,
        },
      },
      response: new Response(),
    } as never);
    renderPage();
    const delBtns = await screen.findAllByRole("button", { name: /删除证书 \d+/ });
    expect((delBtns[0] as HTMLButtonElement).disabled).toBe(true);
    expect((delBtns[1] as HTMLButtonElement).disabled).toBe(false);
    expect((delBtns[2] as HTMLButtonElement).disabled).toBe(false);
  }, 20000);

  it("二次确认后删除调用 DELETE 并刷新", async () => {
    vi.mocked(client.GET).mockResolvedValueOnce({
      data: {
        data: {
          items: [
            { id: 1, domains: ["example.com"], fingerprint: "fp-1", key_algorithm: "ECDSA P-256", description: null, not_after: new Date(Date.now() + 90 * 86400000).toISOString(), revoked_at: null, archived_at: new Date().toISOString(), updated_at: "2026-09-19T00:00:00Z" },
          ],
          total: 1,
          page: 1,
          page_size: 15,
        },
      },
      response: new Response(),
    } as never);
    const del = vi.mocked(client.DELETE);
    del.mockResolvedValueOnce({ data: { data: { deleted: true } }, response: new Response() } as never);
    renderPage();
    const btn = (await screen.findAllByRole("button", { name: /删除证书 1/ }))[0];
    fireEvent.click(btn);
    await screen.findAllByText("确认删除证书？");
    const confirms = await screen.findAllByRole("button", { name: /删\s*除/ });
    fireEvent.click(confirms[confirms.length - 1]);
    await waitFor(() => {
      expect(del).toHaveBeenCalledWith("/api/certificates/1", expect.objectContaining({ params: { path: { id: 1 } } }));
    });
    await waitFor(() => {
      expect(mockedGet.mock.calls.length).toBeGreaterThanOrEqual(2);
    });
  }, 20000);
});
