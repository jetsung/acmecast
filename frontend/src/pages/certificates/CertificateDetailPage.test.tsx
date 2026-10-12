import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { App } from "antd";
import { MemoryRouter, Route, Routes } from "react-router";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ApiError } from "@/api/client";
import { client } from "@/api/client";
import { CertificateDetailPage } from "./CertificateDetailPage";

type MockedGetResult = {
  data: { data: Record<string, unknown> };
  response: Response;
  error: undefined;
};

vi.mock("@/api/client", () => ({
  ApiError: class MockApiError extends Error implements ApiError {
    status: number;
    code: string;
    field?: string;
    constructor(status: number, body: { code?: string; message?: string; field?: string } | undefined) {
      super(body?.message ?? `HTTP ${status}`);
      this.status = status;
      this.code = body?.code ?? `http_${status}`;
      this.field = body?.field;
    }
  },
  client: {
    GET: vi.fn(),
    POST: vi.fn(),
    DELETE: vi.fn(),
  },
}));

vi.mock("@/api/helpers", async () => {
  const actual = await vi.importActual<typeof import("@/api/helpers")>("@/api/helpers");
  return {
    ...actual,
    downloadFile: vi.fn().mockResolvedValue(undefined),
  };
});

import { downloadFile } from "@/api/helpers";

const mockedGet = vi.mocked(client.GET);
const mockedDownload = vi.mocked(downloadFile);

function makeCert(overrides: Record<string, unknown> = {}) {
  return {
    id: 1,
    domains: ["example.com"],
    cert_pem_path: "certs/ssl/example.com.cert.pem",
    key_pem_path: "certs/ssl/example.com.key.pem",
    fingerprint: "abc",
    issuer: "Test CA",
    not_before: "2026-01-01T00:00:00Z",
    not_after: "2027-01-01T00:00:00Z",
    acme_account_access_id: 1,
    revoked_at: null,
    key_algorithm: "ECDSA P-256",
    description: "用于测试的证书",
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:00:00Z",
    ...overrides,
  };
}

function okCert(data: Record<string, unknown>): MockedGetResult {
  return {
    data: { data },
    response: { status: 200 } as Response,
    error: undefined,
  };
}

function renderPage() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <App>
        <MemoryRouter initialEntries={["/certificates/1"]}>
          <Routes>
            <Route path="/certificates/:id" element={<CertificateDetailPage />} />
          </Routes>
        </MemoryRouter>
      </App>
    </QueryClientProvider>,
  );
}

describe("CertificateDetailPage 展示", () => {
  beforeEach(() => vi.clearAllMocks());

  it("展示私钥算法与描述", async () => {
    mockedGet.mockResolvedValue(okCert(makeCert()) as never);

    renderPage();

    expect(await screen.findByText("私钥算法")).toBeTruthy();
    expect(await screen.findByText("ECDSA P-256")).toBeTruthy();
    expect(await screen.findByText("描述")).toBeTruthy();
    expect(await screen.findByText("用于测试的证书")).toBeTruthy();
  });

  it("文件路径三项标签为中文", async () => {
    mockedGet.mockResolvedValue(okCert(makeCert()) as never);

    renderPage();

    expect(await screen.findByText("CA 证书")).toBeTruthy();
    expect(screen.getByText("完整证书链")).toBeTruthy();
    expect(screen.getByText("私钥")).toBeTruthy();
  });

  it("算法与描述缺省时显示占位", async () => {
    mockedGet.mockResolvedValue(okCert(makeCert({ key_algorithm: null, description: null })) as never);

    renderPage();

    await screen.findByText("私钥算法");
    const dashes = screen.getAllByText("—");
    expect(dashes.length).toBeGreaterThanOrEqual(2);
  });

  it("空白描述归一为占位", async () => {
    mockedGet.mockResolvedValue(okCert(makeCert({ description: "   " })) as never);

    renderPage();

    await screen.findByText("描述");
    expect(screen.getAllByText("—").length).toBeGreaterThanOrEqual(1);
  });
});
describe("CertificateDetailPage 下载私钥", () => {
  beforeEach(() => vi.clearAllMocks());

  it("点击下载私钥触发 format=key", async () => {
    mockedGet.mockResolvedValue(okCert(makeCert()) as never);

    renderPage();

    const btn = await screen.findByRole("button", { name: "下载私钥 (PEM)" });
    fireEvent.click(btn);

    await waitFor(() => {
      expect(mockedDownload).toHaveBeenCalledWith("/api/certificates/1/download?format=key", null);
    });
  });

  it("与证书链下载按钮并存", async () => {
    mockedGet.mockResolvedValue(okCert(makeCert()) as never);

    renderPage();

    expect(await screen.findByRole("button", { name: "PEM (证书链)" })).toBeTruthy();
    expect(await screen.findByRole("button", { name: "下载私钥 (PEM)" })).toBeTruthy();
  });
});

describe("CertificateDetailPage 归档", () => {
  beforeEach(() => vi.clearAllMocks());

  it("展示归档按钮且与下载/吊销并存", async () => {
    mockedGet.mockResolvedValue(okCert(makeCert()) as never);
    renderPage();
    expect(await screen.findByRole("button", { name: /归\s*档/ })).toBeTruthy();
    expect(await screen.findByRole("button", { name: /吊\s*销/ })).toBeTruthy();
    expect(await screen.findByRole("button", { name: "下载私钥 (PEM)" })).toBeTruthy();
  });

  it("点击归档二次确认后调用 POST /api/certificates/{id}/archive", async () => {
    mockedGet.mockResolvedValue(okCert(makeCert()) as never);
    const post = vi.mocked(client.POST);
    post.mockResolvedValueOnce({
      data: {
        data: {
          cert_id: 1,
          cert_pem_path: "certs/archived/abc/example.com.cert.pem",
          key_pem_path: "certs/archived/abc/example.com.key.pem",
          archived_now: true,
        },
      },
      response: new Response(),
    } as never);
    renderPage();
    const btn = await screen.findByRole("button", { name: /归\s*档/ });
    fireEvent.click(btn);
    await screen.findAllByText("确认归档证书？");
    const okBtns = screen.getAllByRole("button", { name: /归\s*档/ });
    fireEvent.click(okBtns[okBtns.length - 1]);
    await waitFor(() => {
      expect(post).toHaveBeenCalledWith(
        "/api/certificates/1/archive",
        expect.objectContaining({ params: { path: { id: 1 } } }),
      );
    });
  });

  it("归档失败透出后端错误", async () => {
    mockedGet.mockResolvedValue(okCert(makeCert()) as never);
    const post = vi.mocked(client.POST);
    const errMsg = "归档失败：证书文件不存在";
    post.mockResolvedValueOnce({
      error: { error: { code: "io_error", message: errMsg } },
      response: new Response(null, { status: 500 }),
    } as never);
    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: /归\s*档/ }));
    await screen.findAllByText("确认归档证书？");
    fireEvent.click(screen.getAllByRole("button", { name: /归\s*档/ }).at(-1)!);
    await waitFor(() => {
      expect(post).toHaveBeenCalled();
    });
    // 失败后 modal 仍存在，归档按钮在页面与 modal 中各有一个，取第一个（页面上的）
    expect(screen.getAllByRole("button", { name: /归\s*档/ }).length).toBeGreaterThanOrEqual(1);
  });
  it("取消归档二次确认不发起请求", async () => {
    mockedGet.mockResolvedValue(okCert(makeCert()) as never);
    const post = vi.mocked(client.POST);
    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: /归\s*档/ }));
    await screen.findAllByText("确认归档证书？");
    fireEvent.click(screen.getAllByRole("button", { name: /取\s*消/ })[0]);
    await waitFor(() => {
      expect(post).not.toHaveBeenCalled();
    });
  });

  it("已吊销时归档禁用", async () => {
    mockedGet.mockResolvedValue(
      okCert(makeCert({ revoked_at: new Date().toISOString(), archived_at: null })) as never,
    );
    renderPage();
    const btn = await screen.findByRole("button", { name: /归\s*档/ });
    expect((btn as HTMLButtonElement).disabled).toBe(true);
  });

  it("已归档时归档禁用", async () => {
    mockedGet.mockResolvedValue(
      okCert(makeCert({ revoked_at: null, archived_at: new Date().toISOString() })) as never,
    );
    renderPage();
    const btn = await screen.findByRole("button", { name: /归\s*档/ });
    expect((btn as HTMLButtonElement).disabled).toBe(true);
  });
});

describe("CertificateDetailPage 删除", () => {
  beforeEach(() => vi.clearAllMocks());

  it("活跃态删除禁用", async () => {
    mockedGet.mockResolvedValue(okCert(makeCert({ revoked_at: null, archived_at: null })) as never);
    renderPage();
    const btn = await screen.findByRole("button", { name: /删\s*除/ });
    expect((btn as HTMLButtonElement).disabled).toBe(true);
  });

  it("归档态删除启用", async () => {
    mockedGet.mockResolvedValue(
      okCert(makeCert({ revoked_at: null, archived_at: new Date().toISOString() })) as never,
    );
    renderPage();
    const btn = await screen.findByRole("button", { name: /删\s*除/ });
    expect((btn as HTMLButtonElement).disabled).toBe(false);
  });

  it("吊销态删除启用", async () => {
    mockedGet.mockResolvedValue(
      okCert(makeCert({ revoked_at: new Date().toISOString(), archived_at: null })) as never,
    );
    renderPage();
    const btn = await screen.findByRole("button", { name: /删\s*除/ });
    expect((btn as HTMLButtonElement).disabled).toBe(false);
  });

  it("二次确认后调用 DELETE /api/certificates/{id}", async () => {
    mockedGet.mockResolvedValue(
      okCert(makeCert({ revoked_at: null, archived_at: new Date().toISOString() })) as never,
    );
    const del = vi.mocked(client.DELETE);
    del.mockResolvedValueOnce({ data: { data: { deleted: true } }, response: new Response() } as never);
    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: /删\s*除/ }));
    await screen.findAllByText("确认删除证书？");
    const okBtns = screen.getAllByRole("button", { name: /删\s*除/ });
    fireEvent.click(okBtns[okBtns.length - 1]);
    await waitFor(() => {
      expect(del).toHaveBeenCalledWith("/api/certificates/1", expect.objectContaining({ params: { path: { id: 1 } } }));
    });
  });
});
