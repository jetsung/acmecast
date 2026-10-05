/**
 * 调度配置页回归用例。
 *
 * 曾经的痛点：cron 预览按东八区计算而后端按 UTC 判定，预览所示时刻
 * 与实际触发相差 8 小时；已有调度没有编辑回填入口；调度页看不到运行态
 * 与触发记录，排障只能直查数据库。这里逐条钉住修复后的行为。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { App } from "antd";
import { MemoryRouter } from "react-router";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ScheduleConfigPage } from "./ScheduleConfigPage";

const SCHEDULE_WITH_BOTH = {
  pipeline_id: 1,
  cron: "0 3 * * *",
  enabled: true,
  catch_up: false,
  renewal_domains: ["example.com"],
  last_triggered_at: "2026-10-01T03:00:00Z",
  next_trigger_at: "2026-10-02T03:00:00Z",
  updated_at: "2026-10-01T03:00:05Z",
};

const SCHEDULE_CRON_ONLY = {
  pipeline_id: 2,
  cron: "0 4 * * *",
  enabled: true,
  catch_up: false,
  renewal_domains: null,
  last_triggered_at: null,
  next_trigger_at: "2026-10-02T04:00:00Z",
  updated_at: "2026-10-01T03:00:05Z",
};

const TRIGGER_LOGS = {
  items: [
    {
      id: 7,
      pipeline_id: 1,
      source: "renewal",
      detail: "证书 example.com 距到期不足 30 天",
      triggered_at: "2026-10-01T09:00:00Z",
    },
  ],
  total: 1,
  page: 1,
  page_size: 10,
};

const mocks = vi.hoisted(() => ({
  get: vi.fn(),
  post: vi.fn(),
}));

vi.mock("@/api/client", () => ({
  ApiError: class ApiError extends Error {},
  client: {
    GET: (...args: unknown[]) => mocks.get(...(args as [])),
    POST: (...args: unknown[]) => mocks.post(...(args as [])),
    PUT: vi.fn(),
    DELETE: vi.fn(),
  },
}));


function defaultGetImplementation(path: string) {
      if (path === "/api/schedules") {
        return {
          data: { data: [SCHEDULE_WITH_BOTH, SCHEDULE_CRON_ONLY] },
          response: new Response(),
        };
      }
      if (path === "/api/pipelines") {
        return {
          data: {
            data: {
              items: [
                { id: 1, name: "签发流水线一" },
                { id: 2, name: "签发流水线二" },
              ],
              total: 2,
              page: 1,
              page_size: 100,
            },
          },
          response: new Response(),
        };
      }
      if (path.startsWith("/api/schedules/trigger-logs")) {
        return { data: { data: TRIGGER_LOGS }, response: new Response() };
      }
      throw new Error(`未预料的请求: ${path}`);
}

beforeEach(() => {
  mocks.get.mockReset();
  mocks.get.mockImplementation(async (path: string) => defaultGetImplementation(path));
  mocks.post.mockReset();
  mocks.post.mockImplementation(async () => ({ data: { data: {} }, response: new Response() }));
});

function renderPage() {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <App>
        <MemoryRouter>
          <ScheduleConfigPage />
        </MemoryRouter>
      </App>
    </QueryClientProvider>,
  );
}

describe("cron 预览", () => {
  it("按 UTC 口径预览下一次触发并标注 UTC", async () => {
    renderPage();
    const cronInput = screen.getByPlaceholderText("0 3 * * *");
    fireEvent.change(cronInput, { target: { value: "0 3 * * *" } });

    // toISOString 恒为 UTC：UTC 03:00 与浏览器时区无关，口径由此钉死。
    await waitFor(() => {
      expect(screen.getByText(/下一次触发.*UTC.*03:00/)).toBeTruthy();
    });
  });

  it("无法解析的表达式展示错误提示", async () => {
    renderPage();
    const cronInput = screen.getByPlaceholderText("0 3 * * *");
    fireEvent.change(cronInput, { target: { value: "not a cron" } });

    await waitFor(() => {
      expect(screen.getByText(/cron 错误/)).toBeTruthy();
    });
  });
});

describe("已有调度展示", () => {
  it("展示下次与上次触发时间，缺失字段显示占位符", async () => {
    renderPage();

    await screen.findByText("流水线 #1");
    // 渲染了时间值（本地时区格式）而不是占位符。
    expect(screen.getAllByText(/下次触发：/)[0].textContent).not.toMatch(/：\s*-\s*$/);
    expect(screen.getAllByText(/上次触发：/)[0].textContent).not.toMatch(/：\s*-\s*$/);
    // 纯 cron 调度从未触发过：上次触发显示占位符而不是报错。
    expect(screen.getAllByText(/上次触发：/)[1].textContent).toMatch(/上次触发：\s*-\s*$/);
  });

  it("展示触发记录列表，含时间、来源与说明", async () => {
    renderPage();

    await screen.findByText("证书 example.com 距到期不足 30 天");
expect(screen.getByText("续期")).toBeTruthy();
  });

  it("无触发记录时展示空态", async () => {
    mocks.get.mockImplementation(async (path: string) => {
      if (path === "/api/schedules") {
        return { data: { data: [SCHEDULE_WITH_BOTH] }, response: new Response() };
      }
      if (path === "/api/pipelines") {
        return {
          data: { data: { items: [], total: 0, page: 1, page_size: 100 } },
          response: new Response(),
        };
      }
      return {
        data: { data: { items: [], total: 0, page: 1, page_size: 10 } },
        response: new Response(),
      };
    });

    renderPage();
    await screen.findByText("流水线 #1");
    expect(await screen.findByText("暂无触发记录")).toBeTruthy();
  });
});

describe("编辑回填", () => {
  it("点击编辑后表单回填该调度的现值并锁定流水线", async () => {
    renderPage();
    const editButton = (await screen.findAllByRole("button", { name: /编\s*辑/ }))[0];
    fireEvent.click(editButton);

    await waitFor(() => {
      const cronInput = document.querySelector("#cron") as HTMLInputElement | null;
      expect(cronInput?.value).toBe("0 3 * * *");
    });
    // 回填内容与行数据一致。
    expect(screen.getByText("example.com")).toBeTruthy();
        // 选中项由该调度的流水线 id 解析出名称（antd 渲染在 .ant-select-content 的 title 上）。
    const selection = document.querySelector(".ant-select-disabled .ant-select-content");
    expect(selection?.getAttribute("title")).toBe("签发流水线一");
    // 编辑模式：流水线选择器锁定。
    expect(
      document.querySelector(".ant-card .ant-select-disabled"),
    ).not.toBeNull();
    expect(screen.getByText(/编辑流水线 #1 的调度/)).toBeTruthy();
  });

  it("修改 cron 后保存，请求体携带该流水线与新值", async () => {
    renderPage();
    const editButton = (await screen.findAllByRole("button", { name: /编\s*辑/ }))[0];
    fireEvent.click(editButton);

    const cronInput = await waitFor(() => {
      const input = document.querySelector("#cron") as HTMLInputElement | null;
      expect(input?.value).toBe("0 3 * * *");
      return input as HTMLInputElement;
    });
    fireEvent.change(cronInput, { target: { value: "0 5 * * *" } });
    fireEvent.click(screen.getByRole("button", { name: /保\s*存\s*调\s*度/ }));

    await waitFor(() => {
      expect(mocks.post).toHaveBeenCalled();
    });
    const body = mocks.post.mock.calls[0][1] as { body: Record<string, unknown> };
    expect(body.body.pipeline_id).toBe(1);
    expect(body.body.cron).toBe("0 5 * * *");
    expect(body.body.enabled).toBe(true);
  });
});

describe("快捷启停", () => {
  it("切换开关以该行字段整体保存并翻转 enabled", async () => {
    renderPage();
    const toggle = await screen.findByRole("switch", {
      name: "流水线 1 的调度启停开关",
    });
    fireEvent.click(toggle);

    await waitFor(() => {
      expect(mocks.post).toHaveBeenCalled();
    });
    const body = mocks.post.mock.calls[0][1] as { body: Record<string, unknown> };
    expect(body.body).toEqual({
      pipeline_id: 1,
      cron: "0 3 * * *",
      enabled: false,
      catch_up: false,
      renewal_domains: ["example.com"],
    });
  });

  it("启停保存失败时提示错误，开关保持原状态", async () => {
    mocks.post.mockImplementation(async () => ({
      error: { error: { code: "validation_error", message: "校验失败" } },
      response: new Response(),
    }));
    renderPage();
    const toggle = await screen.findByRole("switch", {
      name: "流水线 1 的调度启停开关",
    });
    fireEvent.click(toggle);

    await waitFor(() => {
      expect(screen.getByText("校验失败")).toBeTruthy();
    });
    // 受控开关：数据未变，开关仍是启用态。
    expect(toggle.className).toContain("ant-switch-checked");
  });
});

describe("触发字段留空语义", () => {
  it("启用的调度两个触发字段均空时阻止提交，不发请求", async () => {
    renderPage();
    // 编辑一条只有 cron 的调度，清空 cron 后提交（renewal 本就为空）。
    const editButton = (await screen.findAllByRole("button", { name: /编\s*辑/ }))[1];
    fireEvent.click(editButton);

    const cronInput = await waitFor(() => {
      const input = document.querySelector("#cron") as HTMLInputElement | null;
      expect(input?.value).toBe("0 4 * * *");
      return input as HTMLInputElement;
    });
    fireEvent.change(cronInput, { target: { value: "" } });
    fireEvent.click(screen.getByRole("button", { name: /保\s*存\s*调\s*度/ }));

    await waitFor(() => {
      expect(
        screen.getByText(/启用的调度必须至少配置 cron 或续期域名集合之一/),
      ).toBeTruthy();
    });
    expect(mocks.post).not.toHaveBeenCalled();
  });
});
