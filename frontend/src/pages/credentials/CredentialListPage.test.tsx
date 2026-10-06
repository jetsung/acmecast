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

/** 从运行中的 8080 抓下来的 acme.account schema（服务端已带 enum、x-required-when 与布局扩展）。 */
const ACME_ACCOUNT_SCHEMA = {
  type: "object",
  // 真实产物的 properties 是字母序（serde_json Map 重排）：eab_hmac_key 会排到
  // eab_kid 前面。渲染顺序由后端注入的 x-field-order 钉住——EAB 密钥对相邻、
  // kid 在前，两个半宽标量流式排成同一行。
  "x-field-order": ["ca", "directory_url", "eab_kid", "eab_hmac_key", "credentials"],
  properties: {
    ca: {
      description: "CA 类型。",
      enum: ["letsencrypt", "letsencrypt-staging", "zerossl", "google", "sslcom", "custom"],
      type: "string",
    },
    credentials: { description: "账号凭据。", type: ["string", "null"], "x-multiline": true },
    directory_url: {
      description: "自定义 Directory URL。",
      type: ["string", "null"],
      "x-required-when": { Equals: { field: "ca", values: ["custom"] } },
      "x-full-width": true,
    },
    eab_hmac_key: { description: "EAB HMAC 密钥。", type: ["string", "null"] },
    eab_kid: { description: "EAB 密钥标识。", type: ["string", "null"] },
  },
  required: ["ca"],
};

const TENCENT_TYPE = {
  type_id: "tencent",
  display_name: "腾讯云（DNS）",
  schema: {
    type: "object",
    // 真实产物的 properties 是字母序（serde_json Map 重排），渲染顺序
    // 由后端注入的 x-field-order 钉住：站点打头，两半密钥成对随后。
    "x-field-order": ["account_site", "secret_id", "secret_key"],
    properties: {
      account_site: {
        description: "账号站点。",
        type: "string",
        enum: ["cn", "intl"],
        // 半宽框独占一行：渲染后补空列占位换行（SSH 主机档案式）。
        "x-end-row": true,
      },
      secret_id: { description: "API 密钥 ID（SecretId）。", type: "string" },
      secret_key: { description: "API 密钥 Key（SecretKey）。", type: "string" },
    },
    required: ["secret_id", "secret_key", "account_site"],
  },
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
          data: { data: [CLOUDFLARE_TYPE, ACME_TYPE, TENCENT_TYPE, SSH_HOST_TYPE] },
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

  it("acme.account：credentials 多行文本域，directory_url 整行，EAB 密钥对同行半宽", async () => {
    await openEditorForRow(1);

    // credentials（含私钥的 JSON）渲染成多行文本域，而不是单行输入框。
    await waitFor(() => {
      expect(document.querySelector("#fields_credentials")?.tagName).toBe("TEXTAREA");
    });

    // 宽度由 schema 扩展驱动：标注 x-full-width 的占整行（24），
    // 未标注的标量半宽（12）成对同行。col 链要跳过 Form.Item 内层的
    // 控制列，从 .ant-form-item 向外找 SchemaForm 渲染的 Col。
    const colOf = (id: string) =>
      document
        .querySelector(id)
        ?.closest(".ant-form-item")
        ?.closest(".ant-col")?.className ?? "";
    expect(colOf("#fields_directory_url")).toContain("ant-col-24");
    expect(colOf("#fields_credentials")).toContain("ant-col-24");
    expect(colOf("#fields_eab_kid")).toContain("ant-col-12");
    expect(colOf("#fields_eab_hmac_key")).toContain("ant-col-12");

    // 渲染顺序由 x-field-order 钉住：真实产物 properties 是字母序（hmac 会
    // 排到 kid 前面），表单里 EAB 密钥对必须相邻且 kid 在前。
    const labels = Array.from(
      document.querySelectorAll<HTMLElement>(".ant-modal .ant-form-item"),
    )
      .map((item) => item.querySelector("label")?.textContent)
      .filter((text): text is string => !!text && !["名称", "类型"].includes(text));
    expect(labels).toEqual(["ca", "directory_url", "eab_kid", "eab_hmac_key", "credentials"]);
  });

  it("腾讯云：account_site 一列宽打头，secret_id 与 secret_key 同为半宽（两列一行）", async () => {
    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: /新\s*建/ }));

    const typeSelect = await waitFor(() => {
      const el = document.querySelector("#type_id");
      expect(el).toBeTruthy();
      return el as HTMLElement;
    });
    fireEvent.mouseDown(typeSelect);
    const typeDropdown = await waitFor(() => {
      const el = document.querySelector(".ant-select-dropdown:not(.ant-select-dropdown-hidden)");
      expect(el).toBeTruthy();
      return el!;
    });
    fireEvent.click(
      typeDropdown.querySelector('.ant-select-item-option[title="腾讯云（DNS）"]')!,
    );

    await waitFor(() => {
      expect(document.querySelector("#fields_secret_id")).toBeTruthy();
    });
    // 与流水线部署配置的布局语义一致：渲染顺序由 x-field-order 钉住——
    // account_site 打头，x-end-row 在行尾补空占位列使其独占一行（框仍
    // 一列宽，SSH 主机档案式）；两半密钥成对两列（cert_path/key_path 式）
    // 排在第二行。
    const items = Array.from(document.querySelectorAll<HTMLElement>(".ant-modal .ant-form-item"));
    const labels = items
      .map((item) => item.querySelector("label")?.textContent)
      .filter((text): text is string => !!text && !["名称", "类型"].includes(text));
    expect(labels).toEqual(["account_site", "secret_id", "secret_key"]);

    const colOf = (id: string) =>
      document
        .querySelector(id)
        ?.closest(".ant-form-item")
        ?.closest(".ant-col")?.className ?? "";
    expect(colOf("#fields_secret_id")).toContain("ant-col-12");
    expect(colOf("#fields_secret_key")).toContain("ant-col-12");
    expect(colOf("#fields_account_site")).toContain("ant-col-12");
    expect(colOf("#fields_account_site")).not.toContain("ant-col-24");
    // 行尾空占位列：把密钥对推到下一行的正是它。
    const spacers = document.querySelectorAll<HTMLElement>(".ant-modal .ant-col[aria-hidden]");
    expect(spacers).toHaveLength(1);
    expect(spacers[0].className).toContain("ant-col-12");
  });
});
