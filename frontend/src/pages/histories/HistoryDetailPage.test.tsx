import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import { App } from "antd";
import { MemoryRouter, Route, Routes } from "react-router";
import { describe, expect, it, vi } from "vitest";
import { HistoryDetailPage } from "./HistoryDetailPage";

vi.mock("@/api/client", () => ({
  client: {
    GET: vi.fn(async (path: string) => {
      if (path.startsWith("/api/histories/") && path.endsWith("/logs")) {
        // 默认由各用例 override
        return { data: { data: [] } };
      }
      if (path.startsWith("/api/histories/")) {
        return {
          data: {
            data: {
              id: 1,
              pipeline_id: 1,
              trigger_source: "manual",
              status: "success",
              started_at: new Date().toISOString(),
              finished_at: new Date().toISOString(),
              error_message: null,
            },
          },
        };
      }
      return { data: { data: [] } };
    }),
  },
  unwrapData: async (result: { data: { data: unknown } }) => result.data.data,
  formatTime: (v: string | null) => v ?? "-",
}));

import { client } from "@/api/client";

const mockedGet = vi.mocked(client.GET);

function renderAtHistoryId(id: string) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <App>
        <MemoryRouter initialEntries={[`/histories/${id}`]}>
          <Routes>
            <Route path="/histories/:id" element={<HistoryDetailPage />} />
          </Routes>
        </MemoryRouter>
      </App>
    </QueryClientProvider>,
  );
}

describe("HistoryDetailPage 腾讯云证书回显", () => {
  it("成功部署回显多证书 ID 芯片", async () => {
    mockedGet.mockImplementation(async (path: string) => {
      if (path.endsWith("/logs")) {
        return {
          data: {
            data: [
              {
                step_index: 1,
                level: "info",
                message: "腾讯云 SSL 部署结果",
                created_at: new Date().toISOString(),
                output: { cert_ids: ["cert-1", "cert-2"], skipped_write: false },
              },
            ],
          },
        } as never;
      }
      return {
        data: {
          data: {
            id: 1,
            pipeline_id: 1,
            trigger_source: "manual",
            status: "success",
            started_at: new Date().toISOString(),
            finished_at: new Date().toISOString(),
            error_message: null,
          },
        },
      } as never;
    });
    renderAtHistoryId("1");
    expect(await screen.findByText("腾讯云证书 ID")).toBeTruthy();
    expect(screen.getByText("cert-1")).toBeTruthy();
    expect(screen.getByText("cert-2")).toBeTruthy();
  });

  it("跳过写入时展示差异化提示而不展示证书芯片", async () => {
    mockedGet.mockImplementation(async (path: string) => {
      if (path.endsWith("/logs")) {
        return {
          data: {
            data: [
              {
                step_index: 1,
                level: "info",
                message: "目标 tencent-ssl 指纹未变，跳过写入；重载已按配置执行",
                created_at: new Date().toISOString(),
                output: { cert_ids: [], skipped_write: true },
              },
            ],
          },
        } as never;
      }
      return {
        data: {
          data: {
            id: 1,
            pipeline_id: 1,
            trigger_source: "manual",
            status: "success",
            started_at: new Date().toISOString(),
            finished_at: new Date().toISOString(),
            error_message: null,
          },
        },
      } as never;
    });

    renderAtHistoryId("1");
    expect(await screen.findByText("指纹一致，已跳过写入")).toBeTruthy();
    expect(screen.queryByText("腾讯云证书 ID")).toBeNull();
  });

  it("无 output 时仅渲染纯文本不出现结构化区块", async () => {
    mockedGet.mockImplementation(async (path: string) => {
      if (path.endsWith("/logs")) {
        return {
          data: {
            data: [
              {
                step_index: 0,
                level: "info",
                message: "普通日志",
                created_at: new Date().toISOString(),
              },
            ],
          },
        } as never;
      }
      return {
        data: {
          data: {
            id: 1,
            pipeline_id: 1,
            trigger_source: "manual",
            status: "success",
            started_at: new Date().toISOString(),
            finished_at: new Date().toISOString(),
            error_message: null,
          },
        },
      } as never;
    });

    renderAtHistoryId("1");
    expect(await screen.findByText(/普通日志/)).toBeTruthy();
    expect(screen.queryByText("腾讯云证书 ID")).toBeNull();
    expect(screen.queryByText("指纹一致，已跳过写入")).toBeNull();
  });

  it("失败运行不误导：展示失败横幅且不因无 cert_ids 误显成功区块", async () => {
    mockedGet.mockImplementation(async (path: string) => {
      if (path.endsWith("/logs")) {
        return {
          data: {
            data: [
              {
                step_index: 1,
                level: "error",
                message: "执行失败",
                created_at: new Date().toISOString(),
              },
            ],
          },
        } as never;
      }
      return {
        data: {
          data: {
            id: 1,
            pipeline_id: 1,
            trigger_source: "manual",
            status: "failed",
            started_at: new Date().toISOString(),
            finished_at: new Date().toISOString(),
            error_message: "第 2 步（cert.deploy）失败：云服务调用失败",
          },
        },
      } as never;
    });
    renderAtHistoryId("1");
    expect(await screen.findByText("失败原因")).toBeTruthy();
    expect(screen.queryByText("腾讯云证书 ID")).toBeNull();
  });
});
