/**
 * 流水线编辑器：schema 驱动的任务输入必须随保存一起提交，且增删排序都不丢/串值。
 *
 * 回归用例：输入值只存在于表单 store（SchemaForm 渲染），不在 `steps` 状态里。
 * 修复要点是保存时按**稳定 id**（而非下标）取回，否则：
 * - 保存时序列化 `steps` 状态 → input 恒为 `{}`，用户填的被静默丢弃；
 * - 用下标当表单路径 → 删除/上移下移后，输入会错配到别的步骤。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { App } from "antd";
import { MemoryRouter, Route, Routes } from "react-router";
import { describe, expect, it, vi } from "vitest";
import { PipelineEditorPage } from "./PipelineEditorPage";

interface PostedStep {
  type_id: string;
  input: Record<string, unknown>;
}

// vi.mock 提升到文件顶部，捕获用的数组必须一并提升。
const posted = vi.hoisted(() => [] as { steps: PostedStep[] }[]);

const TASKS = [
  { type_id: "test.a", display_name: "任务 A" },
  { type_id: "test.b", display_name: "任务 B" },
  { type_id: "test.c", display_name: "任务 C" },
  { type_id: "test.d", display_name: "任务 D" },
  { type_id: "test.e", display_name: "任务 E" },
  { type_id: "test.f", display_name: "任务 F" },
];

vi.mock("@/api/client", () => ({
  ApiError: class ApiError extends Error {},
  client: {
    GET: vi.fn(async (path: string, init?: { params?: { path?: { type_id?: string } } }) => {
      if (path === "/api/tasks") {
        return { data: { data: TASKS }, response: new Response() };
      }
      if (path === "/api/credentials") {
        return {
          data: {
            data: {
              items: [
                { id: 7, name: "le-prod", type_id: "acme.account" },
                { id: 8, name: "some-dns", type_id: "cloudflare" },
                { id: 9, name: "ali-dns", type_id: "aliyun" },
                { id: 10, name: "web-01", type_id: "ssh" },
              ],
              total: 4,
              page: 1,
              page_size: 100,
            },
          },
          response: new Response(),
        };
      }
      if (path === "/api/tasks/{type_id}/schema") {
        // 不同类型的字段名不同，便于断言输入有没有错配到别的步骤。
        const typeId = init?.params?.path?.type_id;
        const fields =
          typeId === "test.b"
            ? // 按 dump 出的真实 cert.apply schema 形态构造:default null/[]、
              // allOf+$ref 枚举、数组字段,用于复现编辑回填丢失。
              {
                domains: {
                  description: "要申请的域名集合。",
                  type: "array",
                  items: { type: "string" },
                },
                challenge: {
                  description: "挑战类型。",
                  allOf: [{ $ref: "#/definitions/ChallengeKind" }],
                },
                account_credential_id: {
                  description: "ACME 账号凭据标识。",
                  type: "integer",
                  format: "int64",
                },
                dns_provider: {
                  description: "DNS 提供商标识。",
                  default: null,
                  type: ["string", "null"],
                },
                dns_credential_id: {
                  description: "DNS 提供商凭据标识。",
                  default: null,
                  type: ["integer", "null"],
                  format: "int64",
                },
                dns_zone: { description: "DNS zone。", default: null, type: ["string", "null"] },
                wait_propagation: { description: "等待传播。", default: true, type: "boolean" },
                contacts: {
                  description: "账号联系人。",
                  default: [],
                  type: "array",
                  items: { type: "string" },
                },
                insecure_skip_verify: { description: "跳过校验。", default: false, type: "boolean" },
              }
            : typeId === "test.c"
              ? { rules: { type: "array", items: { type: "object" } } }
              : typeId === "test.d"
                ? // 真实 cert.store 形态:可空整型 + default null,走凭据下拉。
                  {
                    acme_account_credential_id: {
                      description: "ACME 账号凭据标识。",
                      default: null,
                      type: ["integer", "null"],
                      format: "int64",
                    },
                  }
                : typeId === "test.e"
                  ? { dns_credential_id: { type: "integer" } }
                : typeId === "test.f"
                  ? {
                      dns_provider: { type: "string" },
                      target: { type: "string" },
                      force: { type: "boolean", description: "强制重写已部署的证书。" },
                      acme_account_credential_id: { type: "integer" },
                      // 模拟后端 cert.deploy 的注入产物：config 按 target 给变体 schema。
                      config: {
                        description: "结构与所选目标一致。",
                        "x-target-schemas": {
                          local: {
                            display_name: "本地文件系统",
                            example: {},
                            schema: {
                              type: "object",
                              required: ["cert_path"],
                              properties: { cert_path: { type: "string" } },
                            },
                          },
                          ssh: {
                            display_name: "SSH 远程主机",
                            example: {},
                            schema: {
                              type: "object",
                              // 键序故意与逻辑顺序不一致:渲染顺序应来自前端注入的
                              // x-field-order,而不是后端 schema 键序。
                              properties: {
                                key_mode: { type: ["string", "null"] },
                                cert_mode: { type: ["string", "null"] },
                                key_path: { type: ["string", "null"] },
                                cert_path: { type: ["string", "null"] },
                                port: { type: ["integer", "null"] },
                                host: { type: ["string", "null"] },
                                reload_command: {
                                  type: ["string", "null"],
                                  description: "写入成功后在远程执行的重载命令。",
                                },
                                credential_id: {
                                  type: ["integer", "null"],
                                  description: "SSH 主机档案凭据的标识。",
                                },
                                auth: {
                                  description: "登录远程主机的方式。",
                                  oneOf: [
                                    {
                                      type: "object",
                                      properties: {
                                        kind: { type: "string", enum: ["private_key"] },
                                        credential_id: {
                                          type: "integer",
                                          description: "私钥凭据的标识。",
                                        },
                                      },
                                    },
                                    {
                                      type: "object",
                                      properties: {
                                        kind: { type: "string", enum: ["password"] },
                                        credential_id: {
                                          type: "integer",
                                          description: "口令凭据的标识。",
                                        },
                                      },
                                    },
                                  ],
                                },
                              },
                            },
                          },
                        },
                      },
                    }
                  : { va: { type: "string" } };
        return {
          data: {
            data: {
              type: "object",
              definitions: {
                ChallengeKind: {
                  description: "挑战类型。",
                  oneOf: [
                    { type: "string", enum: ["dns-01"] },
                    { type: "string", enum: ["http-01"] },
                  ],
                },
              },
              properties: fields,
            },
          },
          response: new Response(),
        };
      }
      if (path === "/api/pipelines/7") {
        return {
          data: {
            data: {
              name: "existing",
              description: null,
              enabled: true,
              steps: [
                { type_id: "test.a", input: { va: "kept" }, enabled: true },
                {
                  type_id: "test.b",
                  input: {
                    domains: ["abc.zzzzy.com"],
                    challenge: "dns-01",
                    account_credential_id: 1,
                    dns_provider: "cloudflare",
                    dns_credential_id: 4,
                    dns_zone: null,
                    wait_propagation: true,
                    contacts: ["i@jetsung.com"],
                    insecure_skip_verify: false,
                  },
                  enabled: true,
                },
                {
                  type_id: "test.d",
                  input: { acme_account_credential_id: 1 },
                  enabled: true,
                },
                {
                  type_id: "test.f",
                  input: {
                    dns_provider: "aliyun",
                    target: "ssh",
                    acme_account_credential_id: 7,
                    config: { credential_id: 10, cert_path: "/srv/ssl/site.crt" },
                  },
                  enabled: true,
                },
              ],
            },
          },
          response: new Response(),
        };
      }
      return { data: { data: {} }, response: new Response() };
    }),
    POST: vi.fn(async (_path: string, init: { body: (typeof posted)[number] }) => {
      posted.push(init.body);
      return { data: { data: {} }, response: new Response() };
    }),
    PUT: vi.fn(async (_path: string, init: { body: (typeof posted)[number] }) => {
      posted.push(init.body);
      return { data: { data: {} }, response: new Response() };
    }),
    DELETE: vi.fn(),
  },
}));

function renderPage(initialPath = "/pipelines/new") {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={queryClient}>
      <App>
        <MemoryRouter initialEntries={[initialPath]}>
          <Routes>
            <Route path="/pipelines/:id" element={<PipelineEditorPage />} />
          </Routes>
        </MemoryRouter>
      </App>
    </QueryClientProvider>,
  );
  return queryClient;
}

/** 等任务列表就绪——新步骤的 type_id 取自它，太早点「添加步骤」会拿到空类型。 */
async function waitForTasks(queryClient: QueryClient) {
  await waitFor(() => expect(queryClient.getQueryState(["tasks"])?.status).toBe("success"));
}

/** 添加一个步骤；给了 label 就把它的任务类型改成该类型。 */
async function addStep(label?: string) {
  fireEvent.click(screen.getByRole("button", { name: "添加步骤" }));
  if (label) {
    const combos = screen.getAllByRole("combobox");
    fireEvent.mouseDown(combos[combos.length - 1]);
    // 可见选项按 title 取（antd v6 另有一份隐藏的无障碍 option 列表）。
    fireEvent.click(await screen.findByTitle(label));
  }
}

/** 点保存并返回提交的 body。 */
async function save(): Promise<(typeof posted)[number]> {
  fireEvent.click(screen.getByRole("button", { name: /保\s*存/ }));
  await waitFor(() => expect(posted).toHaveLength(1));
  return posted[0];
}

describe("PipelineEditorPage", () => {
  it("保存时带上任务输入的实际值", async () => {
    posted.length = 0;
    await waitForTasks(renderPage());

    await addStep();
    fireEvent.change(await screen.findByLabelText("名称"), { target: { value: "daily" } });
    fireEvent.change(await screen.findByLabelText("va"), { target: { value: "filled" } });

    const body = await save();
    expect(body.steps[0].input).toEqual({ va: "filled" });
  });

  it("编辑模式回填已存输入，保存时不丢", async () => {
    posted.length = 0;
    await waitForTasks(renderPage("/pipelines/7"));

    const input = await screen.findByLabelText<HTMLInputElement>("va");
    await waitFor(() => expect(input.value).toBe("kept"));

    const body = await save();
    expect(body.steps[0].input).toEqual({ va: "kept" });
    expect(body.steps[1].input).toEqual({
      domains: ["abc.zzzzy.com"],
      challenge: "dns-01",
      account_credential_id: 1,
      dns_provider: "cloudflare",
      dns_credential_id: 4,
      dns_zone: null,
      wait_propagation: true,
      contacts: ["i@jetsung.com"],
      insecure_skip_verify: false,
    });
    expect(body.steps[2].input).toEqual({ acme_account_credential_id: 1 });
    expect(body.steps[3].input).toEqual({
      dns_provider: "aliyun",
      target: "ssh",
      acme_account_credential_id: 7,
      config: { credential_id: 10, cert_path: "/srv/ssl/site.crt" },
    });
  });

  it("切换任务类型会清空该步骤的输入", async () => {
    posted.length = 0;
    await waitForTasks(renderPage());
    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "p" } });

    await addStep();
    fireEvent.change(await screen.findByLabelText("va"), { target: { value: "stale" } });

    // 把这一步的类型从 A 改成 B。
    fireEvent.mouseDown(screen.getByRole("combobox"));
    fireEvent.click(await screen.findByTitle("任务 B"));
    // 原字段已随类型切换移除，避免上一类型的残留值混入。
    expect(screen.queryByLabelText("va")).toBeNull();

    const body = await save();
    expect(body.steps[0].type_id).toBe("test.b");
    expect(body.steps[0].input).toEqual({});
  });

  it("删除前面的步骤，其余步骤的输入不会错配（稳定 id，而非下标）", async () => {
    posted.length = 0;
    await waitForTasks(renderPage());
    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "p" } });

    await addStep();
    fireEvent.change(await screen.findByLabelText("va"), { target: { value: "A" } });
    await addStep("任务 C");
    fireEvent.change(await screen.findByLabelText("rules"), { target: { value: '[{"key":"C"}]' } });

    // 删掉第 1 步：若输入按步骤下标挂，剩下的第 2 步会错读到 A。
    fireEvent.click(screen.getAllByRole("button", { name: /移\s*除/ })[0]);

    const body = await save();
    expect(body.steps).toHaveLength(1);
    expect(body.steps[0].type_id).toBe("test.c");
    expect(body.steps[0].input).toEqual({ rules: [{ key: "C" }] });
  });

  it("上移/下移后输入跟随各自的步骤", async () => {
    posted.length = 0;
    await waitForTasks(renderPage());
    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "p" } });

    await addStep();
    fireEvent.change(await screen.findByLabelText("va"), { target: { value: "A" } });
    await addStep("任务 C");
    fireEvent.change(await screen.findByLabelText("rules"), { target: { value: '[{"key":"C"}]' } });

    fireEvent.click(screen.getAllByRole("button", { name: /下\s*移/ })[0]);

    const body = await save();
    expect(body.steps.map((step) => [step.type_id, step.input])).toEqual([
      ["test.c", { rules: [{ key: "C" }] }],
      ["test.a", { va: "A" }],
    ]);
  });

  it("JSON 对象数组输入端到端保存为解析后的结构", async () => {
    posted.length = 0;
    await waitForTasks(renderPage());
    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "p" } });

    await addStep("任务 C");
    fireEvent.change(await screen.findByLabelText("rules"), { target: { value: '[{"key":"v"}]' } });

    const body = await save();
    expect(body.steps[0].input).toEqual({ rules: [{ key: "v" }] });
  });

  it("acme_account_credential_id 渲染成按名称选择的下拉，只列 acme.account 凭据", async () => {
    posted.length = 0;
    const queryClient = renderPage();
    await waitForTasks(queryClient);
    // 下拉选项来自凭据列表，等它加载完再操作步骤。
    await waitFor(() => expect(queryClient.getQueryState(["credentials"])?.status).toBe("success"));
    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "p" } });

    await addStep("任务 D");

    const field = await screen.findByLabelText("acme_account_credential_id");
    expect(field.getAttribute("role")).toBe("combobox");
    fireEvent.mouseDown(field);
    // 只应出现 acme.account 的凭据（some-dns 是 cloudflare 类型，不该出现）。
    expect(screen.queryByTitle("some-dns（id=8）")).toBeNull();
    fireEvent.click(await screen.findByTitle("le-prod（id=7）"));

    const body = await save();
    // 值保持数字 ID。
    expect(body.steps[0].input).toEqual({ acme_account_credential_id: 7 });
  });

  it("dns_credential_id 渲染成下拉，只列 DNS 提供商凭据", async () => {
    posted.length = 0;
    const queryClient = renderPage();
    await waitForTasks(queryClient);
    await waitFor(() => expect(queryClient.getQueryState(["credentials"])?.status).toBe("success"));
    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "p" } });

    await addStep("任务 E");

    const field = await screen.findByLabelText("dns_credential_id");
    expect(field.getAttribute("role")).toBe("combobox");
    fireEvent.mouseDown(field);
    // acme.account 的账号凭据不该出现；cloudflare / aliyun 应出现。
    expect(screen.queryByTitle("le-prod（id=7）")).toBeNull();
    expect(screen.getByTitle("some-dns（id=8）")).toBeTruthy();
    fireEvent.click(await screen.findByTitle("ali-dns（id=9）"));

    const body = await save();
    expect(body.steps[0].input).toEqual({ dns_credential_id: 9 });
  });

  it("按字段语义选控件：枚举/凭据走下拉，任意结构走 JSON 文本", async () => {
    posted.length = 0;
    const queryClient = renderPage();
    await waitForTasks(queryClient);
    await waitFor(() => expect(queryClient.getQueryState(["credentials"])?.status).toBe("success"));
    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "p" } });

    await addStep("任务 F");

    // dns_provider：固定的提供商标识，选「阿里云」。
    const provider = await screen.findByLabelText("dns_provider");
    expect(provider.getAttribute("role")).toBe("combobox");
    fireEvent.mouseDown(provider);
    fireEvent.click(await screen.findByTitle("阿里云"));

    // target：固定的部署目标，选「SSH 远程主机」。
    const target = screen.getByLabelText("target");
    expect(target.getAttribute("role")).toBe("combobox");
    fireEvent.mouseDown(target);
    fireEvent.click(await screen.findByTitle("SSH 远程主机"));

    // 凭据 ID 字段（cert.store 用的字段名）同样走下拉。
    const credential = screen.getByLabelText("acme_account_credential_id");
    expect(credential.getAttribute("role")).toBe("combobox");
    fireEvent.mouseDown(credential);
    fireEvent.click(await screen.findByTitle("le-prod（id=7）"));

    // config 按所选 target 渲染成结构化子表单；SSH 变体已收紧：
    // 连接与认证信息全部来自所选的主机档案凭据，表单不提供直填入口。
    const hostCredential = await screen.findByLabelText("SSH 主机档案");
    expect(hostCredential.getAttribute("role")).toBe("combobox");
    fireEvent.mouseDown(hostCredential);
    // 类型过滤本身由 fromCredentials 断言（见 dns_credential_id 用例）；
    // 这里 antd 关闭的下拉仍留隐藏 option 在 DOM，不能全局断言「不出现」。
    fireEvent.click(await screen.findByTitle("web-01（id=10）"));

    // 直填字段（可从档案获得或由缺省兜底）不再渲染，只留部署输入。
    for (const banned of ["host", "port", "user", "auth", "cert_mode", "key_mode"]) {
      expect(screen.queryByLabelText(banned)).toBeNull();
    }
    expect(screen.getByLabelText("cert_path")).toBeTruthy();
    expect(screen.getByLabelText("key_path")).toBeTruthy();
    expect(screen.getByLabelText("reload_command")).toBeTruthy();

    // 根级顺序:force/target 两个决策字段排最前,config(SSH 主机档案)跟后。
    const allItems = Array.from(document.querySelectorAll<HTMLElement>(".ant-form-item"));
    const rootOrder = allItems
      .map((item) => item.querySelector("label")?.textContent)
      .filter((text): text is string => text !== null);
    expect(rootOrder.indexOf("target")).toBeLessThan(rootOrder.indexOf("force"));
    expect(rootOrder.indexOf("force")).toBeLessThan(rootOrder.indexOf("SSH 主机档案"));

    // 布局:SSH 主机档案与 reload_command 各独占一行(右侧空列占位),
    // cert_path/key_path 成对同行(半宽)。
    const colOf = (label: string) =>
      allItems.find((item) => item.querySelector("label")?.textContent === label)
        ?.closest(".ant-col");
    expect(colOf("SSH 主机档案")?.className).toContain("ant-col-12");
    expect(colOf("cert_path")?.className).toContain("ant-col-12");
    expect(colOf("key_path")?.className).toContain("ant-col-12");
    expect(colOf("reload_command")?.className).toContain("ant-col-12");
    // 空列占位:档案与重载命令所在行的行尾各有一个无 label 的空列。
    const emptyCols = document.querySelectorAll<HTMLElement>(".ant-col[aria-hidden]");
    expect(emptyCols.length).toBe(2);

    fireEvent.change(screen.getByLabelText("cert_path"), { target: { value: "/srv/ssl/site.crt" } });
    fireEvent.change(screen.getByLabelText("key_path"), { target: { value: "/srv/ssl/site.key" } });

    const body = await save();
    expect(body.steps[0].input).toEqual({
      dns_provider: "aliyun",
      target: "ssh",
      acme_account_credential_id: 7,
      // 提交只含档案引用与部署输入,连接/权限字段由档案在执行时提供。
      config: { credential_id: 10, cert_path: "/srv/ssl/site.crt", key_path: "/srv/ssl/site.key" },
    });
  });
});