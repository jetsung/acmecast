import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { App } from "antd";
import { MemoryRouter } from "react-router";
import { describe, expect, it, vi } from "vitest";
import { client } from "@/api/client";
import { CredentialListPage } from "./CredentialListPage";

const CLOUDFLARE_TYPE = {
  type_id: "cloud.cloudflare",
  display_name: "Cloudflare",
  schema: {
    type: "object",
    // 从运行中的 8080 抓取的真实注入形态：schemars 对 Option<String> 不产
    // enum 数组，enum 与 x-enum-labels/x-visible-when 均由后端手动注入。
    "x-field-order": ["api_token", "proxy_mode", "proxy_url"],
    properties: {
      api_token: { type: "string", description: "API Token" },
      proxy_mode: {
        type: "string",
        description: "出网代理模式。",
        enum: ["direct", "inherit", "custom"],
        "x-enum-labels": { direct: "直连", inherit: "继承全局", custom: "自定义" },
      },
      proxy_url: {
        type: "string",
        description: "自定义出网代理。",
        "x-visible-when": { Equals: { field: "proxy_mode", values: ["custom"] } },
      },
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
      // 固定半宽独占一行：长渠道名不被挤压隐藏（x-end-row → 半宽 + 空占位列）。
      "x-end-row": true,
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
  type_id: "cloud.tencent",
  display_name: "腾讯云",
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
  type_id: "deploy.ssh",
  display_name: "SSH 主机",
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

/** S3 兼容存储类型：schema 含后端注入的 x-field-order（properties 本身是字母序）。 */
const S3_TYPE = {
  type_id: "deploy.s3",
  display_name: "S3 兼容存储",
  schema: {
    type: "object",
    properties: {
      access_key: { type: "string", description: "Access Key。" },
      secret_key: { type: "string", description: "Secret Key。" },
      session_token: { type: ["string", "null"], description: "Session Token（临时凭据，可选）。" },
      endpoint: { type: "string", description: "Endpoint。" },
      bucket: { type: "string", description: "存储桶。" },
      region: { type: "string", description: "区域。" },
      prefix: { type: ["string", "null"], description: "对象键前缀（可选）。" },
    },
    "x-field-order": [
      "access_key",
      "secret_key",
      "endpoint",
      "bucket",
      "region",
      "prefix",
      "session_token",
    ],
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
                  type_id: "cloud.cloudflare",
                  capabilities: ["dns", "deploy"],
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
              page_size: 15,
            },
          },
          response: new Response(),
        };
      }
      if (path === "/api/credential-types") {
        return {
          data: { data: [CLOUDFLARE_TYPE, ACME_TYPE, TENCENT_TYPE, SSH_HOST_TYPE, S3_TYPE] },
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
              type_id: isAcme ? "acme.account" : "cloud.cloudflare",
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

  it("Cloudflare：proxy_mode 渲染为中文下拉，custom 时才显示 proxy_url", async () => {
    await openEditorForRow(0);

    // proxy_mode 是枚举下拉（存量记录未选模式时回填为空）。
    const modeInput = await waitFor(() => {
      const el = document.querySelector<HTMLInputElement>("#fields_proxy_mode");
      expect(el?.getAttribute("role")).toBe("combobox");
      return el!;
    });

    // 打开下拉：三个候选项展示后端注入的中文标签。
    fireEvent.mouseDown(modeInput);
    const dropdown = document.querySelector(
      ".ant-select-dropdown:not(.ant-select-dropdown-hidden)",
    )!;
    const optionTitles = [
      ...dropdown.querySelectorAll(".ant-select-item-option-content"),
    ].map((el) => el.textContent);
    for (const label of ["直连", "继承全局", "自定义"]) {
      expect(optionTitles).toContain(label);
    }

    // proxy_mode=custom 之前 proxy_url 隐藏（x-visible-when 联动）。
    expect(document.querySelector("#fields_proxy_url")).toBeNull();

    // 选中「自定义」（候选项 title 为中文 label）：proxy_url 输入框出现。
    const customOption = [
      ...dropdown.querySelectorAll(".ant-select-item-option"),
    ].find((el) => el.textContent === "自定义");
    fireEvent.click(customOption!);
    await waitFor(() => {
      expect(document.querySelector("#fields_proxy_url")).toBeTruthy();
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
      typeDropdown.querySelector('.ant-select-item-option[title="SSH 主机"]')!,
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

  it("acme.account：ca 固定半宽独占一行，directory_url/credentials 各占整行，EAB 密钥对同行半宽", async () => {
    await openEditorForRow(1);

    await waitFor(() => {
      expect(document.querySelector("#fields_credentials")?.tagName).toBe("TEXTAREA");
    });

    expect(
      document.querySelector("#fields_ca")?.closest(".ant-form-item")?.closest(".ant-col")?.className ?? "",
    ).toContain("ant-col-12");
    expect(document.querySelector(".ant-modal .ant-col[aria-hidden].ant-col-12")).toBeTruthy();
    expect(
      document.querySelector("#fields_directory_url")?.closest(".ant-form-item")?.closest(".ant-col")
        ?.className ?? "",
    ).toContain("ant-col-24");
    expect(
      document.querySelector("#fields_credentials")?.closest(".ant-form-item")?.closest(".ant-col")
        ?.className ?? "",
    ).toContain("ant-col-24");
    expect(
      document.querySelector("#fields_eab_kid")?.closest(".ant-form-item")?.closest(".ant-col")
        ?.className ?? "",
    ).toContain("ant-col-12");
    expect(
      document.querySelector("#fields_eab_hmac_key")?.closest(".ant-form-item")?.closest(".ant-col")
        ?.className ?? "",
    ).toContain("ant-col-12");

    const labels = Array.from(
      document.querySelectorAll<HTMLElement>(".ant-modal .ant-form-item"),
    )
      .map((item) => item.querySelector("label")?.textContent)
      .filter(
        (text): text is string =>
          !!text && !["名称", "类型", "可用权限", "备注"].includes(text),
      );
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
      typeDropdown.querySelector('.ant-select-item-option[title="腾讯云"]')!,
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
      .filter(
        (text): text is string =>
          !!text && !["名称", "类型", "可用权限", "备注"].includes(text),
      );
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

  it("S3 兼容存储：字段按 x-field-order 渲染，三组成对字段两两成行，session_token 置末", async () => {
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
      typeDropdown.querySelector('.ant-select-item-option[title="S3 兼容存储"]')!,
    );

    await waitFor(() => {
      expect(document.querySelector("#fields_access_key")).toBeTruthy();
    });
    // 渲染顺序由 x-field-order 钉住（mock 的 properties 是字母序），
    // 三组成对字段相邻成对，session_token 保持半宽、自然落末尾。
    const items = Array.from(document.querySelectorAll<HTMLElement>(".ant-modal .ant-form-item"));
    const labels = items
      .map((item) => item.querySelector("label")?.textContent)
      .filter(
        (text): text is string =>
          !!text && !["名称", "类型", "可用权限", "备注"].includes(text),
      );
    expect(labels).toEqual([
      "access_key",
      "secret_key",
      "endpoint",
      "bucket",
      "region",
      "prefix",
      "session_token",
    ]);

    const adjacent = (a: string, b: string) =>
      labels.indexOf(b) === labels.indexOf(a) + 1;
    expect(adjacent("access_key", "secret_key")).toBe(true);
    expect(adjacent("endpoint", "bucket")).toBe(true);
    expect(adjacent("region", "prefix")).toBe(true);
    expect(labels[labels.length - 1]).toBe("session_token");

    // 前六字段均为半宽（两列一行自然成对），无整行/行尾占位扩展。
    const colOf = (id: string) =>
      document
        .querySelector(id)
        ?.closest(".ant-form-item")
        ?.closest(".ant-col")?.className ?? "";
    for (const id of [
      "#fields_access_key",
      "#fields_secret_key",
      "#fields_endpoint",
      "#fields_bucket",
      "#fields_region",
      "#fields_prefix",
      "#fields_session_token",
    ]) {
      expect(colOf(id)).toContain("ant-col-12");
      expect(colOf(id)).not.toContain("ant-col-24");
    }
    expect(
      document.querySelectorAll(".ant-modal .ant-col[aria-hidden]"),
    ).toHaveLength(0);
  });

  it("列表“可用权限”列居左，多能力以「DNS、部署」字符串展示", async () => {
    renderPage();
    // 列不设 align：表头与单元格都不应有 antd 注入的居中内联样式。
    const th = await waitFor(() => {
      const el = [...document.querySelectorAll<HTMLElement>("thead th")].find(
        (cell) => cell.textContent === "可用权限",
      );
      expect(el).toBeTruthy();
      return el!;
    });
    expect(th.style.textAlign).not.toBe("center");
    // 受控类型行：能力以「、」连接的字符串居左展示（等行数据渲染完成）。
    const td = await waitFor(() => {
      const el = [...document.querySelectorAll<HTMLElement>("tbody td")].find(
        (cell) => cell.textContent === "DNS、部署",
      );
      expect(el).toBeTruthy();
      return el!;
    });
    expect(td.style.textAlign).not.toBe("center");
    expect(td.querySelector(".ant-tag")).toBeNull();
  });

  it("连续编辑两条不同凭据:第二次弹窗不含第一条的字段残留", async () => {
    renderPage();
    const editButtons = await screen.findAllByRole("button", { name: /编\s*辑/ });
    fireEvent.click(editButtons[0]);
    await waitFor(() => {
      expect((document.querySelector("#fields_api_token") as HTMLInputElement | null)?.value).toBe(
        "secret-tok",
      );
    });
    // 不关闭弹窗直接切换到第二条：验证“切换编辑目标→重置并回填新记录”
    // 对应用户在同一弹窗打开状态下连续点不同行的“编辑”
    fireEvent.click(editButtons[1]);
    await waitFor(() => {
      const nameInput = document.querySelector("#name") as HTMLInputElement | null;
      expect(nameInput?.value).toBe("le-account");
    });
    await waitFor(() => {
      expect(document.querySelector("#fields_ca")).toBeTruthy();
      expect(document.querySelector("#fields_api_token")).toBeNull();
    });
    vi.mocked(client.PUT).mockClear();
    fireEvent.click(document.querySelector(".ant-modal .ant-btn-primary")!);
    await waitFor(() => {
      expect(client.PUT).toHaveBeenCalledTimes(1);
    });
    const putCall = vi.mocked(client.PUT).mock.calls[0] as [
      string,
      { body: { fields: Record<string, unknown> } },
    ];
    const putFields = putCall[1].body.fields;
    expect(putFields).not.toHaveProperty("api_token");
    expect(putFields).toHaveProperty("ca", "letsencrypt");
  });

  it("详情加载期间不展示上一条记录的旧值且保存被禁用", async () => {
    let resolveDetail: (v: unknown) => void = () => {};
    const delayedDetail = new Promise<unknown>((resolve) => {
      resolveDetail = resolve as (v: unknown) => void;
    });
    const originalGet = vi.mocked(client.GET);
    // mock 的 GET 返回分支较多，vi.mocked 的窄化类型在这里不便精确表达，
    // 用宽松签名承接并回放原实现。
    const baseGet = originalGet.getMockImplementation() as unknown as (
      path: string,
    ) => Promise<unknown>;
    vi.mocked(client.GET).mockImplementation((async (path: string) => {
      if (path === "/api/credentials/5") {
        await delayedDetail;
        return {
          data: {
            data: {
              id: 5,
              name: "cf-main",
              type_id: "cloud.cloudflare",
              fields: { api_token: "secret-tok" },
              created_at: "2026-09-19T00:00:00Z",
              updated_at: "2026-09-19T00:00:00Z",
            },
          },
          response: new Response(),
        };
      }
      return baseGet(path);
    }) as unknown as typeof client.GET);
    try {
      renderPage();
      const editButtons = await screen.findAllByRole("button", { name: /编\s*辑/ });
      fireEvent.click(editButtons[0]);
      await waitFor(() => {
        expect(document.querySelector(".ant-modal")).toBeTruthy();
      });
      await waitFor(() => {
        const okBtn = document.querySelector(".ant-modal .ant-btn-primary") as HTMLButtonElement | null;
        expect(okBtn?.disabled).toBe(true);
      });
      resolveDetail({});
      await waitFor(() => {
        expect((document.querySelector("#name") as HTMLInputElement | null)?.value).toBe("cf-main");
      });
      await waitFor(() => {
        const okBtn = document.querySelector(".ant-modal .ant-btn-primary") as HTMLButtonElement | null;
        expect(okBtn?.disabled).toBe(false);
      });
    } finally {
      vi.mocked(client.GET).mockImplementation(
        baseGet as unknown as typeof client.GET,
      );
    }
  });
});
