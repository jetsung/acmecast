/**
 * 凭据页「编辑」回填回归用例。
 *
 * 曾经的 bug：弹窗只记住了行 id 与类型，从不拉取该条记录的现值——
 * 表单永远空白，而 PUT 是整体替换语义，用户直接点保存会把已存密钥清成空。
 * 修复要点：编辑打开时 GET /api/credentials/{id}（带解密字段值）并回填
 * name / type_id / fields。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { App } from "antd";
import { MemoryRouter } from "react-router";
import { describe, expect, it, vi } from "vitest";
import { CredentialListPage } from "./CredentialListPage";

const CLOUDFLARE_TYPE = {
  type_id: "cloudflare",
  display_name: "Cloudflare（DNS）",
  schema: {
    type: "object",
    properties: {
      api_token: { type: "string", description: "API Token" },
    },
    required: ["api_token"],
  },
};

/** 从运行中的 8080 抓下来的 acme.account schema（服务端已带 enum 与 x-required-when）。 */
const ACME_ACCOUNT_SCHEMA = {
  type: "object",
  properties: {
    ca: {
      description: "CA 类型。",
      enum: ["letsencrypt", "letsencrypt-staging", "zerossl", "google", "sslcom", "custom"],
      type: "string",
    },
    directory_url: {
      description: "自定义 Directory URL。",
      type: ["string", "null"],
      "x-required-when": { Equals: { field: "ca", values: ["custom"] } },
    },
    eab_kid: { description: "EAB 密钥标识。", type: ["string", "null"] },
    eab_hmac_key: { description: "EAB HMAC 密钥。", type: ["string", "null"] },
    credentials: { description: "账号凭据。", type: ["string", "null"] },
  },
  required: ["ca"],
};

const ACME_TYPE = {
  type_id: "acme.account",
  display_name: "ACME 账号",
  schema: ACME_ACCOUNT_SCHEMA,
};

/** SSH 主机类型:schema 键序故意打乱,渲染顺序应来自前端注入的 x-field-order。 */
const SSH_HOST_TYPE = {
  type_id: "ssh",
  display_name: "SSH 主机（部署）",
  schema: {
    type: "object",
    properties: {
      private_key: { type: ["string", "null"], "x-multiline": true, description: "私钥 PEM。" },
      password: { type: ["string", "null"], description: "口令。" },
      user: { type: "string", description: "登录用户。" },
      port: { type: ["integer", "null"], description: "端口。" },
      host: { type: "string", description: "主机。" },
      key_mode: { type: ["string", "null"], description: "私钥权限。" },
      cert_mode: { type: ["string", "null"], description: "证书权限。" },
    },
  },
};

vi.mock("@/api/client", () => ({
  ApiError: class ApiError extends Error {},
  client: {
    GET: vi.fn(async (path: string) => {
      if (path === "/api/credentials") {
        return {
          data: {
            data: {
              items: [
                {
                  id: 5,
                  name: "cf-main",
                  type_id: "cloudflare",
                  created_at: "2026-09-19T00:00:00Z",
                  updated_at: "2026-09-19T00:00:00Z",
                },
                {
                  id: 6,
                  name: "le-account",
                  type_id: "acme.account",
                  created_at: "2026-09-19T00:00:00Z",
                  updated_at: "2026-09-19T00:00:00Z",
                },
              ],
              total: 2,
              page: 1,
              page_size: 20,
            },
          },
          response: new Response(),
        };
      }
      if (path === "/api/credential-types") {
        return {
          data: { data: [CLOUDFLARE_TYPE, ACME_TYPE, SSH_HOST_TYPE] },
          response: new Response(),
        };
      }
      if (path.startsWith("/api/credentials/")) {
        // 详情接口带回解密后的字段值（编辑回填的唯一来源）。
        const isAcme = path === "/api/credentials/6";
        return {
          data: {
            data: {
              id: isAcme ? 6 : 5,
              name: isAcme ? "le-account" : "cf-main",
              type_id: isAcme ? "acme.account" : "cloudflare",
              fields: isAcme
                ? { ca: "letsencrypt", credentials: '{"id":"acct/1"}' }
                : { api_token: "secret-tok" },
              created_at: "2026-09-19T00:00:00Z",
              updated_at: "2026-09-19T00:00:00Z",
            },
          },
          response: new Response(),
        };
      }
      throw new Error(`未预料的请求: ${path}`);
    }),
    POST: vi.fn(),
    PUT: vi.fn(async () => ({ data: {}, response: new Response() })),
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
          <CredentialListPage />
        </MemoryRouter>
      </App>
    </QueryClientProvider>,
  );
}

describe("凭据编辑弹窗", () => {
  async function openEditorForRow(index: number) {
    renderPage();
    // antd 会在两字中文按钮里插空格（「编 辑」），用正则匹配。
    const editButtons = await screen.findAllByRole("button", { name: /编\s*辑/ });
    fireEvent.click(editButtons[index]);
  }

  it("点编辑后用该条记录的现值回填表单", async () => {
    await openEditorForRow(0);

    // 弹窗渲染在 body 门户里，从 document 查询回填结果。
    await waitFor(() => {
      const nameInput = document.querySelector(
        "#name",
      ) as HTMLInputElement | null;
      expect(nameInput?.value).toBe("cf-main");
    });

    // schema 驱动的字段（fields.api_token）也回填。
    await waitFor(() => {
      const tokenInput = document.querySelector(
        "#fields_api_token",
      ) as HTMLInputElement | null;
      expect(tokenInput?.value).toBe("secret-tok");
    });
  });

  it("acme.account：ca 渲染为内置 CA 下拉，custom 时 directory_url 转必填", async () => {
    const { client } = await import("@/api/client");
    await openEditorForRow(1);

    // ca 是下拉（combobox），且回填了记录现值 letsencrypt。
    const caInput = await waitFor(() => {
      const el = document.querySelector<HTMLInputElement>("#fields_ca");
      expect(el?.getAttribute("role")).toBe("combobox");
      expect(
        el?.closest(".ant-select")?.textContent,
      ).toContain("letsencrypt");
      return el!;
    });

    // 打开下拉：应列出服务端 schema 下发的全部内置别名 + custom。
    fireEvent.mouseDown(caInput);
    const dropdown = document.querySelector(
      ".ant-select-dropdown:not(.ant-select-dropdown-hidden)",
    )!;
    const optionTitles = [...dropdown.querySelectorAll(".ant-select-item-option-content")].map(
      (el) => el.textContent,
    );
    for (const alias of ["letsencrypt", "zerossl", "custom"]) {
      expect(optionTitles).toContain(alias);
    }
    fireEvent.click(dropdown.querySelector('.ant-select-item-option[title="custom"]')!);

    // custom + 未填 directory_url（该记录本就没有此字段）：
    // 保存应被条件必填拦下，不发出 PUT。
    await waitFor(() => {
      expect(
        document.querySelector("#fields_directory_url[aria-required='true']"),
      ).toBeTruthy();
    });
    fireEvent.click(document.querySelector(".ant-modal .ant-btn-primary")!);
    expect(await screen.findByText("请填写 directory_url")).toBeTruthy();
    expect(client.PUT).not.toHaveBeenCalled();

    // 换回内置 CA：directory_url 不再是必填（星号消失）。
    fireEvent.mouseDown(caInput);
    const dropdown2 = document.querySelector(
      ".ant-select-dropdown:not(.ant-select-dropdown-hidden)",
    )!;
    fireEvent.click(
      dropdown2.querySelector('.ant-select-item-option[title="letsencrypt"]') ?? dropdown2,
    );
    await waitFor(() => {
      expect(
        document.querySelector("#fields_directory_url[aria-required='true']"),
      ).toBeNull();
    });
  });

  it("SSH 主机类型:字段按前端声明的顺序渲染,成对字段相邻", async () => {
    renderPage();
    // 编辑模式下类型选择器被禁用,走「新建凭据」流程选类型。
    fireEvent.click(await screen.findByRole("button", { name: /新\s*建/ }));

    // 选择类型为 SSH 主机(mock 的 schema 键序是故意打乱的)。
    const typeSelect = await waitFor(() => {
      const el = document.querySelector("#type_id");
      expect(el).toBeTruthy();
      return el as HTMLElement;
    });
    fireEvent.mouseDown(typeSelect);
    const typeDropdown = await waitFor(() => {
      const el = document.querySelector(
        ".ant-select-dropdown:not(.ant-select-dropdown-hidden)",
      );
      expect(el).toBeTruthy();
      return el!;
    });
    fireEvent.click(
      typeDropdown.querySelector('.ant-select-item-option[title="SSH 主机（部署）"]')!,
    );

    // 打开类型下拉→选中 SSH 主机后,字段卡按 SSH_HOST_FIELD_ORDER 渲染:
    // cert_mode/key_mode、host/port、user/password 相邻,private_key 最后。
    await waitFor(() => {
      expect(document.querySelector("#fields_host")).toBeTruthy();
    });
    const items = Array.from(document.querySelectorAll<HTMLElement>(".ant-modal .ant-form-item"));
    const labels = items
      .map((item) => item.querySelector("label")?.textContent)
      .filter((text): text is string => text !== null);
    const adjacent = (a: string, b: string) =>
      labels.indexOf(b) === labels.indexOf(a) + 1;
    expect(adjacent("cert_mode", "key_mode")).toBe(true);
    expect(adjacent("host", "port")).toBe(true);
    expect(adjacent("user", "password")).toBe(true);
    expect(labels[labels.length - 1]).toBe("private_key");
  });
});
