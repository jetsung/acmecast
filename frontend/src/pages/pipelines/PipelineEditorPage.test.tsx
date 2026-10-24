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
import { fetchTencentSslCertificates } from "@/api/client";
import { PipelineEditorPage } from "./PipelineEditorPage";

interface PostedStep {
  type_id: string;
  input: Record<string, unknown>;
}

interface PostedBody {
  steps: PostedStep[];
  /** 运行结束简讯渠道（通知类凭据 ID 数组）。 */
  notification_channels?: number[];
}

// vi.mock 提升到文件顶部，捕获用的数组必须一并提升。
const posted = vi.hoisted(() => [] as PostedBody[]);

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
  fetchTencentSslCertificates: vi.fn(async (_credentialId: number) => [
    { value: "CERTA", label: "CERTA (idev.top - idev)" },
    { value: "CERTB", label: "CERTB (example.com)" },
  ]),
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
                { id: 7, name: "le-prod", type_id: "acme.account", remark: "", proxy_mode: "direct" },
                { id: 8, name: "some-dns", type_id: "cloud.cloudflare", remark: "", proxy_mode: "direct" },
                { id: 9, name: "ali-dns", type_id: "cloud.aliyun", remark: "", proxy_mode: "direct" },
                { id: 10, name: "web-01", type_id: "deploy.ssh", remark: "", proxy_mode: "direct" },
                { id: 30, name: "腾讯云 SSL", type_id: "cloud.tencent", remark: "生产 DNS", proxy_mode: "direct" },
                { id: 31, name: "EdgeOne DNS", type_id: "dns.tencent-eo", remark: "", proxy_mode: "direct" },
                { id: 40, name: "SSH 主机", type_id: "deploy.ssh", remark: "", proxy_mode: "direct" },
                { id: 41, name: "jump-01", type_id: "deploy.ssh", remark: "", proxy_mode: "direct" },
                { id: 50, name: "ops-feishu", type_id: "notify.feishu", remark: "", proxy_mode: "direct" },
                { id: 51, name: "ops-mail", type_id: "notify.email", remark: "", proxy_mode: "direct" },
              ],
              total: 10,
              page: 1,
              page_size: 100,
            },
          },
          response: new Response(),
        };
      }
      if (path === "/api/credential-types") {
        return {
          data: {
            data: [
              { type_id: "notify.feishu", display_name: "飞书机器人", schema: {} },
              { type_id: "notify.email", display_name: "邮件", schema: {} },
              { type_id: "cloud.aliyun", display_name: "阿里云 DNS", schema: {} },
            ],
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
                // 与 cert.apply 真实产物一致：allOf+$ref 枚举 + 缺省值 + 展示名注入。
                key_algorithm: {
                  description: "申请密钥与证书的算法；缺省 ECDSA P-256。",
                  allOf: [{ $ref: "#/definitions/CertKeyAlgorithm" }],
                  default: "ecdsa_p256",
                  "x-enum-labels": {
                    ecdsa_p256: "ECDSA P-256",
                    ecdsa_p384: "ECDSA P-384",
                    ed25519: "Ed25519",
                    rsa2048: "RSA 2048",
                  },
                },
                account_credential_id: {
                  description: "ACME 账号凭据标识。",
                  type: "integer",
                  format: "int64",
                },
                dns_credential_id: {
                  description: "DNS 提供商凭据标识。",
                  default: null,
                  type: ["integer", "null"],
                  format: "int64",
                },
                // 后端 input_schema() 注入的多域名必填条件（与 cert.apply 真实产物一致）。
                dns_zone: {
                  description: "DNS zone。",
                  default: null,
                  type: ["string", "null"],
                  "x-required-when": { MinItems: { field: "domains", count: 2 } },
                },
                wait_propagation: { description: "等待传播。", default: true, type: "boolean" },
                contacts: {
                  description: "账号联系人。",
                  default: [],
                  type: "array",
                  items: { type: "string" },
                },
                insecure_skip_verify: { description: "跳过校验。", default: false, type: "boolean" },
                renew_before_days: {
                  description: "到期前 N 天自动续期。",
                  default: null,
                  type: ["integer", "null"],
                },
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
                              required: ["fullchain_path", "key_path"],
                              // 键序故意与逻辑顺序不一致：渲染顺序来自前端注入的
                              // x-field-order——key/fullchain/ca 三行两列，左路径右权限。
                              properties: {
                                key_mode: { type: ["string", "null"] },
                                uid: { type: ["integer", "null"] },
                                ca_mode: { type: ["string", "null"] },
                                fullchain_path: { type: ["string", "null"] },
                                key_path: { type: ["string", "null"] },
                                cert_mode: { type: ["string", "null"] },
                                ca_path: { type: ["string", "null"] },
                                gid: { type: ["integer", "null"] },
                                reload_command: { type: ["string", "null"] },
                              },
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
                                fullchain_path: { type: ["string", "null"] },
                                ca_path: { type: ["string", "null"] },
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
                                bastion_credential_id: {
                                  type: ["integer", "null"],
                                  description: "跳板机凭据的标识。",
                                  default: null,
                                  format: "int64",
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
                          // 腾讯云 SSL 变体：与后端注入产物同构（凭据引用、备注、重复开关、替换开关、CLB 同步）。
                          "tencent-ssl": {
                            display_name: "腾讯云 SSL 证书",
                            example: {},
                            schema: {
                              type: "object",
                              properties: {
                                credential_id: {
                                  type: "integer",
                                  description: "腾讯云凭据的标识。",
                                  "x-credential-types": ["cloud.tencent"],
                                },
                                alias: {
                                  type: "string",
                                  description: "上传到云端的证书备注名。",
                                },
                                certificate_id: {
                                  type: ["string", "null"],
                                  description: "云端已存在证书 ID（可选），非空时直接写入该证书。",
                                },
                                repeatable: {
                                  type: "boolean",
                                  description: "允许重复上传同指纹证书。",
                                },
                              },
                              "x-field-order": [
                                "credential_id",
                                "alias",
                                "certificate_id",
                                "repeatable",
                              ],
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
                CertKeyAlgorithm: {
                  description: "申请密钥与证书的算法类型。",
                  oneOf: [
                    { type: "string", enum: ["ecdsa_p256"] },
                    { type: "string", enum: ["ecdsa_p384"] },
                    { type: "string", enum: ["ed25519"] },
                    { type: "string", enum: ["rsa2048"] },
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
              notification_channels: [50, 51],
              steps: [
                { type_id: "test.a", input: { va: "kept" }, enabled: true },
                {
                  type_id: "test.b",
                  input: {
                    domains: ["abc.zzzzy.com"],
                    challenge: "dns-01",
                    key_algorithm: "ecdsa_p384",
                    account_credential_id: 1,
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
                    dns_provider: "cloud.aliyun",
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

function renderPage(initialPath = "/pipelines/new", state?: unknown) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={queryClient}>
      <App>
        <MemoryRouter initialEntries={[{ pathname: initialPath, state }]}>
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
    expect(body.steps[1].input).toEqual(
      expect.objectContaining({
        domains: ["abc.zzzzy.com"],
        challenge: "dns-01",
        key_algorithm: "ecdsa_p384",
        account_credential_id: 1,
        dns_credential_id: 4,
        dns_zone: null,
        wait_propagation: true,
        contacts: ["i@jetsung.com"],
        insecure_skip_verify: false,
      }),
    );
    // key_algorithm 的下拉按已存值回显展示名。
    expect(await screen.findByTitle("ECDSA P-384")).toBeTruthy();
    expect(body.steps[2].input).toEqual({ acme_account_credential_id: 1 });
    expect(body.steps[3].input).toEqual({
      dns_provider: "cloud.aliyun",
      target: "ssh",
      acme_account_credential_id: 7,
      // 目标档案非空,跳板字段渲染后 SchemaDefault 把 default null 写入 store,
      // 保存时随 config 提交(null 与缺失等价)。
      config: { credential_id: 10, bastion_credential_id: null, cert_path: "/srv/ssl/site.crt" },
    });
  });

  it("切换任务类型会清空该步骤的输入", async () => {
    posted.length = 0;
    await waitForTasks(renderPage());
    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "p" } });

    await addStep();
    fireEvent.change(await screen.findByLabelText("va"), { target: { value: "stale" } });

    // 把这一步的类型从 A 改成 B（最后一个 combobox 是步骤的类型下拉，
    // 页面上方还有「通知」多选）。
    const combos = screen.getAllByRole("combobox");
    fireEvent.mouseDown(combos[combos.length - 1]);
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

  it("通知多选仅列通知类凭据，保存时提交 ID 数组", async () => {
    posted.length = 0;
    const queryClient = renderPage();
    await waitForTasks(queryClient);
    await waitFor(() => expect(queryClient.getQueryState(["credentials"])?.status).toBe("success"));
    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "p" } });

    const field = await screen.findByLabelText("通知");
    fireEvent.mouseDown(field);
    // 非通知类凭据（DNS / 部署 / 账号）不出现在「通知」下拉里。
    expect(screen.queryByTitle("some-dns（id=8）")).toBeNull();
    expect(screen.queryByTitle("web-01（id=10）")).toBeNull();

    /**
     * 打开「通知」下拉的面板（关闭的下拉会留在 DOM，jsdom 里退场动画不结束，
     * 所以按 aria-owns 取所属面板而非依赖可见性）。
     */
    const openNotificationPanel = async (trigger: HTMLElement) => {
      fireEvent.mouseDown(trigger);
      return await waitFor(() => {
        const listId = trigger.getAttribute("aria-owns");
        expect(listId).toBeTruthy();
        const list = document.getElementById(listId!);
        expect(list).toBeTruthy();
        return list!.closest<HTMLElement>(".ant-select-dropdown")!;
      });
    };
    // 选项由短格式 label 定位（option 自身的 title 取自 label），
    // 返回的是渲染后的内容元素，即下拉列表实际显示的文字。
    const findNotificationOptionContent = async (
      panel: HTMLElement,
      shortLabel: string,
    ) => {
      const option = await waitFor(() => {
        const found = panel.querySelector<HTMLElement>(
          `.ant-select-item-option[title="${shortLabel}"]`,
        );
        expect(found).toBeTruthy();
        return found!;
      });
      return option.querySelector<HTMLElement>(".ant-select-item-option-content")!;
    };
    // 已选标签（选中后显示在选择框内）的文本集合。
    const selectedTagTexts = () =>
      Array.from(
        document.querySelectorAll<HTMLElement>(".ant-select-selection-item"),
      ).map((element) => element.textContent ?? "");

    const panel = await openNotificationPanel(field);
    const feishuContent = await findNotificationOptionContent(panel, "ops-feishu（id=50）");
    // 下拉列表展开时仍为完整格式（含渠道类型）。
    expect(feishuContent.textContent).toContain("ops-feishu（id=50）— 飞书机器人");
    const mailContent = await findNotificationOptionContent(panel, "ops-mail（id=51）");
    expect(mailContent.textContent).toContain("ops-mail（id=51）— 邮件");

    // 选项由短格式 label 选中，两个都选入。
    fireEvent.click(await screen.findByTitle("ops-feishu（id=50）"));
    fireEvent.click(await screen.findByTitle("ops-mail（id=51）"));

    // 选中后标签为短格式，不再包含渠道类型。
    const selectedTexts = selectedTagTexts();
    expect(selectedTexts).toContain("ops-feishu（id=50）");
    expect(selectedTexts).toContain("ops-mail（id=51）");
    expect(selectedTexts.some((text) => text.includes("—"))).toBe(false);

    const body = await save();
    // 多选值保持数字 ID 数组。
    expect(body.notification_channels).toEqual([50, 51]);
  });

  it("编辑模式回显已选通知渠道", async () => {
    posted.length = 0;
    await waitForTasks(renderPage("/pipelines/7"));

    // 已选渠道以选中项标签回显：短格式，不含渠道类型。
    const selectedTagTexts = () =>
      Array.from(
        document.querySelectorAll<HTMLElement>(".ant-select-selection-item"),
      ).map((element) => element.textContent ?? "");
    await waitFor(() => {
      expect(selectedTagTexts()).toContain("ops-feishu（id=50）");
    });
    expect(selectedTagTexts()).toContain("ops-mail（id=51）");
    expect(selectedTagTexts().some((text) => text.includes("—"))).toBe(false);
  });

  it("复制流水线时预填通知渠道", async () => {
    posted.length = 0;
    await waitForTasks(
      renderPage("/pipelines/new", {
        copyFrom: {
          name: "existing_copy",
          description: null,
          enabled: true,
          notification_channels: [50],
          steps: [{ type_id: "test.a", input: { va: "kept" }, enabled: true }],
        },
      }),
    );

    // 复制入时预填：选中项同样只显示短格式。
    const selectedTagTexts = () =>
      Array.from(
        document.querySelectorAll<HTMLElement>(".ant-select-selection-item"),
      ).map((element) => element.textContent ?? "");
    await waitFor(() => {
      expect(selectedTagTexts()).toContain("ops-feishu（id=50）");
    });
  });

  it("按字段语义选控件：枚举/凭据走下拉，任意结构走 JSON 文本", async () => {
    posted.length = 0;
    const queryClient = renderPage();
    await waitForTasks(queryClient);
    await waitFor(() => expect(queryClient.getQueryState(["credentials"])?.status).toBe("success"));
    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "p" } });

    await addStep("任务 F");

    // target：固定的部署目标，选「SSH 远程主机」。
    const target = await screen.findByLabelText("target");
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
    for (const banned of ["host", "port", "user", "auth", "cert_mode", "key_mode", "cert_path"]) {
      expect(screen.queryByLabelText(banned)).toBeNull();
    }
    expect(screen.getByLabelText("fullchain_path")).toBeTruthy();
    expect(screen.getByLabelText("ca_path")).toBeTruthy();
    expect(screen.getByLabelText("key_path")).toBeTruthy();
    expect(screen.getByLabelText("reload_command")).toBeTruthy();
    // 目标主机档案选定后跳板字段渲染出来（未选档案时隐藏，见联动用例）。
    expect(screen.getByLabelText("跳板机")).toBeTruthy();

    // 根级顺序:target 决策字段排最前,config(SSH 主机档案)跟后;
    // force 已被注入 x-hidden,不再渲染。
    const allItems = Array.from(document.querySelectorAll<HTMLElement>(".ant-form-item"));
    const rootOrder = allItems
      .map((item) => item.querySelector("label")?.textContent)
      .filter((text): text is string => text !== null);
    expect(rootOrder).not.toContain("force");
    expect(rootOrder.indexOf("target")).toBeLessThan(rootOrder.indexOf("SSH 主机档案"));

    // 布局:SSH 主机档案、跳板机各独占一行(右侧空列占位),
    // key/fullchain 与 ca/reload_command 两两成对同行(半宽)。
    const colOf = (label: string) =>
      allItems.find((item) => item.querySelector("label")?.textContent === label)
        ?.closest(".ant-col");
    expect(colOf("SSH 主机档案")?.className).toContain("ant-col-12");
    expect(colOf("跳板机")?.className).toContain("ant-col-12");
    expect(colOf("key_path")?.className).toContain("ant-col-12");
    expect(colOf("fullchain_path")?.className).toContain("ant-col-12");
    expect(colOf("ca_path")?.className).toContain("ant-col-12");
    expect(colOf("reload_command")?.className).toContain("ant-col-12");
    // 空列占位:档案与跳板所在行的行尾各有一个无 label 的空列。
    const emptyCols = document.querySelectorAll<HTMLElement>(".ant-col[aria-hidden]");
    expect(emptyCols.length).toBe(2);

    // 渲染顺序随 SSH_FIELDS(注入的 x-field-order):key→fullchain→ca→reload_command。
    const fieldOrder = ["key_path", "fullchain_path", "ca_path", "reload_command"].map((label) =>
      rootOrder.indexOf(label),
    );
    expect(fieldOrder[0]).toBeLessThan(fieldOrder[1]);
    expect(fieldOrder[1]).toBeLessThan(fieldOrder[2]);
    expect(fieldOrder[2]).toBeLessThan(fieldOrder[3]);

    fireEvent.change(screen.getByLabelText("fullchain_path"), { target: { value: "/srv/ssl/site.fullchain.cer" } });
    fireEvent.change(screen.getByLabelText("ca_path"), { target: { value: "/srv/ssl/site.ca.cer" } });
    fireEvent.change(screen.getByLabelText("key_path"), { target: { value: "/srv/ssl/site.key" } });

    const body = await save();
    expect(body.steps[0].input).toEqual({
      target: "ssh",
      acme_account_credential_id: 7,
      // 提交只含档案引用与部署输入,连接/权限字段由档案在执行时提供;
      // 跳板字段渲染后 SchemaDefault 把 default null 写入 store,随保存提交。
      config: {
        credential_id: 10,
        bastion_credential_id: null,
        fullchain_path: "/srv/ssl/site.fullchain.cer",
        ca_path: "/srv/ssl/site.ca.cer",
        key_path: "/srv/ssl/site.key",
      },
    });
  });

  it("local 变体三行两列：key/fullchain/ca 各占一行，左路径右权限", async () => {
    posted.length = 0;
    const queryClient = renderPage();
    await waitForTasks(queryClient);
    await waitFor(() => expect(queryClient.getQueryState(["credentials"])?.status).toBe("success"));
    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "p" } });

    await addStep("任务 F");

    const target = await screen.findByLabelText("target");
    fireEvent.mouseDown(target);
    fireEvent.click(await screen.findByTitle("本地文件系统"));

    // 渲染顺序来自 LOCAL_FIELD_ORDER（而非 schema 键序）：key 行在上，
    // 每行左路径右权限。
    const order = [
      "key_path",
      "key_mode",
      "fullchain_path",
      "cert_mode",
      "ca_path",
      "ca_mode",
      "uid",
      "gid",
      "reload_command",
    ];
    // 切换 target 后子表单异步重渲，先等第一个字段出现。
    await screen.findByLabelText(order[0]);
    for (const label of order) {
      expect(screen.getByLabelText(label)).toBeTruthy();
    }

    const allItems = Array.from(document.querySelectorAll<HTMLElement>(".ant-form-item"));
    const labels = allItems
      .map((item) => item.querySelector("label")?.textContent)
      .filter((text): text is string => text !== null);
    const indexes = order.map((label) => labels.indexOf(label));
    expect(indexes.every((index) => index >= 0)).toBe(true);
    for (let i = 1; i < indexes.length; i += 1) {
      expect(indexes[i - 1]).toBeLessThan(indexes[i]);
    }

    // 九个字段都是半宽：相邻两个（左路径右权限）成对占满一行。
    const colOf = (label: string) =>
      allItems.find((item) => item.querySelector("label")?.textContent === label)
        ?.closest(".ant-col");
    for (const label of order) {
      expect(colOf(label)?.className).toContain("ant-col-12");
    }
  });

  it("跳板机字段随目标主机档案联动：未选隐藏、选中出现、切换目标刷新选项", async () => {
    posted.length = 0;
    const queryClient = renderPage();
    await waitForTasks(queryClient);
    await waitFor(() => expect(queryClient.getQueryState(["credentials"])?.status).toBe("success"));
    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "p" } });

    await addStep("任务 F");

    const target = await screen.findByLabelText("target");
    fireEvent.mouseDown(target);
    fireEvent.click(await screen.findByTitle("SSH 远程主机"));

    // 未选目标主机档案：跳板字段不渲染。
    await screen.findByLabelText("SSH 主机档案");
    expect(screen.queryByLabelText("跳板机")).toBeNull();

    /**
     * 打开某个 Select 的下拉面板：用 combobox 的 aria-owns 找到它自己的
     * listbox，再取所属面板——关闭的下拉会留在 DOM（jsdom 里退场动画不结束），
     * 不能用「可见面板」来猜归属。
     */
    const openPanel = async (trigger: HTMLElement) => {
      fireEvent.mouseDown(trigger);
      return await waitFor(() => {
        const listId = trigger.getAttribute("aria-owns");
        expect(listId).toBeTruthy();
        const list = document.getElementById(listId!);
        expect(list).toBeTruthy();
        const panel = list!.closest<HTMLElement>(".ant-select-dropdown");
        expect(panel).toBeTruthy();
        return panel!;
      });
    };

    /** 打开 Select 并在它自己的面板里点选 option（选项由虚拟列表异步渲染，需重试）。 */
    const chooseOption = async (trigger: HTMLElement, title: string) => {
      const panel = await openPanel(trigger);
      const option = await waitFor(() => {
        const found = panel.querySelector<HTMLElement>(
          `.ant-select-item-option[title="${title}"]`,
        );
        expect(found).toBeTruthy();
        return found!;
      });
      console.error("PANEL_CLASS:", panel.className, "| OPTION:", option.outerHTML.slice(0, 200), "| TRIGGER_ID:", trigger.id, trigger.getAttribute("aria-owns"));
      fireEvent.click(option);
      console.error("AFTER_CLICK_SEL:", JSON.stringify(Array.from(document.querySelectorAll(".ant-select-selection-item")).map((item) => item.textContent)));
    };

    // 选 web-01（id=10）后跳板字段出现。
    const hostCredential = screen.getByLabelText("SSH 主机档案");
    await chooseOption(hostCredential, "web-01（id=10）");
    const bastion = await screen.findByLabelText("跳板机");
    expect(bastion.getAttribute("role")).toBe("combobox");

    // 跳板选项：排除当前目标档案 id=10，列出其他 SSH 档案 id=40/41。
    const bastionPanel = await openPanel(bastion);
    await waitFor(() => {
      expect(
        bastionPanel.querySelector('.ant-select-item-option[title="SSH 主机（id=40）"]'),
      ).toBeTruthy();
    });
    expect(
      bastionPanel.querySelector('.ant-select-item-option[title="web-01（id=10）"]'),
    ).toBeNull();
    fireEvent.click(
      bastionPanel.querySelector('.ant-select-item-option[title="jump-01（id=41）"]')!,
    );

    // 切换目标档案到 id=40：跳板选项即时刷新——排除新的目标、纳入原目标。
    await chooseOption(hostCredential, "SSH 主机（id=40）");
    const refreshedPanel = await openPanel(bastion);
    await waitFor(() => {
      expect(
        refreshedPanel.querySelector('.ant-select-item-option[title="web-01（id=10）"]'),
      ).toBeTruthy();
    });
    expect(
      refreshedPanel.querySelector('.ant-select-item-option[title="SSH 主机（id=40）"]'),
    ).toBeNull();

    const body = await save();
    const config = body.steps[0].input.config as Record<string, unknown>;
    // 已选跳板值保留：目标引用切换不影响跳板选择。
    expect(config["credential_id"]).toBe(40);
    expect(config["bastion_credential_id"]).toBe(41);
  });

  it("跳板机下拉可清空：选中后能退回未选，保存提交 null", async () => {
    posted.length = 0;
    const queryClient = renderPage();
    await waitForTasks(queryClient);
    await waitFor(() => expect(queryClient.getQueryState(["credentials"])?.status).toBe("success"));
    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "p" } });

    await addStep("任务 F");

    const target = await screen.findByLabelText("target");
    fireEvent.mouseDown(target);
    fireEvent.click(await screen.findByTitle("SSH 远程主机"));

    const hostCredential = await screen.findByLabelText("SSH 主机档案");
    fireEvent.mouseDown(hostCredential);
    fireEvent.click(await screen.findByTitle("web-01（id=10）"));

    // 选跳板 jump-01（id=41）：下拉带 allowClear 后才出现清空按钮。
    // 用 aria-owns 锁定跳板下拉自己的面板再点选，避免与残留的档案下拉面板
    // （同样含 jump-01 选项）撞上「多个元素同 title」。
    const bastion = await screen.findByLabelText("跳板机");
    fireEvent.mouseDown(bastion);
    const bastionPanel = await waitFor(() => {
      const listId = bastion.getAttribute("aria-owns");
      expect(listId).toBeTruthy();
      const panel = document
        .getElementById(listId!)
        ?.closest<HTMLElement>(".ant-select-dropdown");
      expect(panel).toBeTruthy();
      return panel!;
    });
    const jumpOption = await waitFor(() => {
      const o = bastionPanel.querySelector<HTMLElement>(
        '.ant-select-item-option[title="jump-01（id=41）"]',
      );
      expect(o).toBeTruthy();
      return o!;
    });
    fireEvent.click(jumpOption);

    const bastionSelect = bastion.closest(".ant-select")!;
    // 选中值后 antd 渲染清空按钮（.ant-select-clear）。
    await waitFor(() => {
      expect(bastionSelect.querySelector(".ant-select-clear")).toBeTruthy();
    });
    // 清空：退回未选，不再携带跳板引用（antd v6 清空按钮由 click 触发）。
    fireEvent.click(bastionSelect.querySelector(".ant-select-clear")!);

    const body = await save();
    const config = body.steps[0].input.config as Record<string, unknown>;
    // 清空后提交为 null 或缺失（allowClear 触发 onChange(undefined)）。
    expect(config["bastion_credential_id"] ?? null).toBeNull();
  });

  it("cert.deploy 的腾讯云 SSL 变体渲染备注与替换开关", async () => {
    posted.length = 0;
    const queryClient = renderPage();
    await waitForTasks(queryClient);
    await waitFor(() => expect(queryClient.getQueryState(["credentials"])?.status).toBe("success"));
    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "p" } });

    await addStep("任务 F");

    const target = await screen.findByLabelText("target");
    fireEvent.mouseDown(target);
    fireEvent.click(await screen.findByTitle("腾讯云 SSL 证书"));

    // 切换 target 清空存量 config 后，腾讯云变体渲染出自己的子表单：
    // 子字段 label 用字段名（说明进 tooltip），备注是输入框；
    // repeatable 是 Switch。
    fireEvent.change(await screen.findByLabelText("alias"), {
      target: { value: "acmecast example.com" },
    });
    expect(screen.getByLabelText("repeatable").getAttribute("role")).toBe("switch");

    // credential_id 按 x-credential-types 仅列 tencent 凭据：/mock 凭据列表里
    // tencent (id=30, name=腾讯云 SSL) 应出现，tencent-eo (id=31) 与 SSH 等不应出现。
    fireEvent.mouseDown(screen.getByLabelText("credential_id"));
    expect(await screen.findByTitle("腾讯云 SSL（id=30）— 生产 DNS")).toBeTruthy();
    expect(screen.queryByTitle("EdgeOne DNS（id=31）")).toBeNull();
    expect(screen.queryByTitle("SSH 主机（id=40）")).toBeNull();

    const body = await save();
    const config = body.steps[0].input.config as Record<string, unknown>;
    expect(body.steps[0].input.target).toBe("tencent-ssl");
    expect(config["alias"]).toBe("acmecast example.com");
  });

  it("tencent-ssl 选中证书 ID 后进入替换模式：repeatable 禁用且值固定，清空后恢复", async () => {
    posted.length = 0;
    const queryClient = renderPage();
    await waitForTasks(queryClient);
    await waitFor(() => expect(queryClient.getQueryState(["credentials"])?.status).toBe("success"));
    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "p" } });

    await addStep("任务 F");
    const target = await screen.findByLabelText("target");
    fireEvent.mouseDown(target);
    fireEvent.click(await screen.findByTitle("腾讯云 SSL 证书"));

    // 「证书 ID」默认渲染为空的单选可清空下拉（x-options 初始为空），旁置「同步」按钮。
    await screen.findByLabelText("certificate_id");
    // antd Button 会在两字文本间自动插空格（渲染为「同 步」），按宽松正则取。
    const syncButton = screen.getByRole("button", { name: /同\s*步/ });
    // 按钮与下拉控件同处一行：二者最近的公共祖先是 flex 行容器，
    // 按钮不换行掉到控件下方。
    const select = screen.getByLabelText("certificate_id").closest(".ant-select");
    expect(select).toBeTruthy();
    expect(syncButton.closest('[style*="display: flex"]')).toBeTruthy();
    expect(select!.closest('[style*="display: flex"]')).toBe(
      syncButton.closest('[style*="display: flex"]'),
    );

    // 先把 repeatable 打开：选中证书 ID 后进入替换模式——开关禁用且值被
    // 固定为 false（repeatable 仅作用于上传流程，替换不携带）。
    fireEvent.click(await screen.findByLabelText("repeatable"));

    /** 打开某个 Select 的下拉面板：用 combobox 的 aria-owns 找它自己的 listbox。 */
    const openPanel = async (trigger: HTMLElement) => {
      fireEvent.mouseDown(trigger);
      return await waitFor(() => {
        const listId = trigger.getAttribute("aria-owns");
        expect(listId).toBeTruthy();
        const list = document.getElementById(listId!);
        expect(list).toBeTruthy();
        const panel = list!.closest<HTMLElement>(".ant-select-dropdown");
        expect(panel).toBeTruthy();
        return panel!;
      });
    };

    /** 打开 Select 并在它自己的面板里点选 option（选项由虚拟列表异步渲染，需重试）。 */
    const chooseOption = async (trigger: HTMLElement, title: string) => {
      const panel = await openPanel(trigger);
      const option = await waitFor(() => {
        const found = panel.querySelector<HTMLElement>(
          `.ant-select-item-option[title="${title}"]`,
        );
        expect(found).toBeTruthy();
        return found!;
      });
      fireEvent.click(option);
    };

    // 选定腾讯云凭据并等选中渲染落地（watch 传播是异步的），再点「同步」。
    await chooseOption(screen.getByLabelText("credential_id"), "腾讯云 SSL（id=30）— 生产 DNS");
    // 等凭据选中渲染落地（antd v6 的选中项 DOM 与 v5 不同，按选中 label 文本断言）。
    await waitFor(() => {
      expect(document.body.textContent?.includes("id=30")).toBe(true);
    });
    fireEvent.click(screen.getByRole("button", { name: /同\s*步/ }));
    await waitFor(() => expect(vi.mocked(fetchTencentSslCertificates)).toHaveBeenCalled());

    // 同步后下拉选项可用：选 CERTA。选中后 repeatable 禁用且值固定。
    await chooseOption(screen.getByLabelText("certificate_id"), "CERTA (idev.top - idev)");
    const repeatSwitch = await waitFor(() => {
      const repeat = screen.getByLabelText("repeatable");
      expect(
        repeat.hasAttribute("disabled") || repeat.getAttribute("aria-disabled") === "true",
      ).toBe(true);
      return repeat;
    });
    expect(repeatSwitch.getAttribute("aria-checked")).toBe("false");

    // 清空证书 ID：开关恢复可编辑。
    const certTrigger = screen.getByLabelText("certificate_id").closest(".ant-select")!;
    fireEvent.mouseEnter(certTrigger);
    fireEvent.click(certTrigger.querySelector(".ant-select-clear")!);
    await waitFor(() => {
      expect(screen.getByLabelText("repeatable").hasAttribute("disabled")).toBe(false);
    });

    // 重新选中并保存：固定值随表单提交（后端另有硬编码兜底）。
    await chooseOption(screen.getByLabelText("certificate_id"), "CERTB (example.com)");
    await waitFor(() => {
      expect(screen.getByLabelText("repeatable").hasAttribute("disabled")).toBe(true);
    });

    const body = await save();
    const config = body.steps[0].input.config as Record<string, unknown>;
    expect(config["certificate_id"]).toBe("CERTB");
    expect(config["repeatable"]).toBe(false);
  });

  /** cert.apply 步骤的 domains 填入两个域名，并等必填联动落地。 */
  async function fillTwoDomains() {
    const domains = await screen.findByLabelText("domains");
    for (const domain of ["a.example.com", "b.example.com"]) {
      fireEvent.change(domains, { target: { value: domain } });
      fireEvent.keyDown(domains, { key: "Enter" });
    }
    // 等联动重渲染落地（dns_zone 出现必填星号）再点保存，避免与
    // Form.useWatch 驱动的规则更新竞态。
    await waitFor(() => {
      expect(
        document.querySelector("input#inputs_1_dns_zone[aria-required='true']"),
      ).toBeTruthy();
    });
  }

  it("cert.apply 多域名留空 dns_zone 被拦截，填写后放行", async () => {
    // 多域名下静默推导 zone 容易踩错（*.skiy.net 曾被推成 net），表单要求
    // 显式配置：domains ≥ 2 时 dns_zone 必填。保存成功会跳转页面，
    // 所以「拦截 → 放行」在同一次渲染里完成，删减场景另起用例。
    posted.length = 0;
    const queryClient = renderPage();
    await waitForTasks(queryClient);
    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "p" } });

    await addStep("任务 B");
    await fillTwoDomains();

    // 留空 dns_zone：保存被拦截，请求不发出。
    fireEvent.click(screen.getByRole("button", { name: /保\s*存/ }));
    expect(await screen.findByText("请填写 dns_zone")).toBeTruthy();
    expect(posted).toHaveLength(0);

    // 填上 zone：放行，提交值带 zone。
    fireEvent.change(screen.getByLabelText("dns_zone"), { target: { value: "example.com" } });
    fireEvent.click(screen.getByRole("button", { name: /保\s*存/ }));
    await waitFor(() => expect(posted).toHaveLength(1));
    expect(posted[0].steps[0].input).toMatchObject({
      domains: ["a.example.com", "b.example.com"],
      dns_zone: "example.com",
    });
  });

  it("cert.apply 密钥类型下拉带缺省与展示名，选择后保存", async () => {
    posted.length = 0;
    const queryClient = renderPage();
    await waitForTasks(queryClient);
    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "p" } });

    await addStep("任务 B");
    // 单域名即可保存——多域名会触发 dns_zone 必填联动，与本用例无关。
    const domains = await screen.findByLabelText("domains");
    fireEvent.change(domains, { target: { value: "a.example.com" } });
    fireEvent.keyDown(domains, { key: "Enter" });

    // schema 的缺省值预填下拉（ECDSA P-256），展开后按展示名渲染选项。
    // 缺省选中项让选择器框本身也带 title，取下拉里的可见选项匹配。
    const field = await screen.findByLabelText("key_algorithm");
    expect(field.getAttribute("role")).toBe("combobox");
    fireEvent.mouseDown(field);
    const option = (await screen.findAllByTitle("ECDSA P-384")).find((element) =>
      element.className.includes("ant-select-item-option"),
    );
    expect(option).toBeTruthy();
    fireEvent.click(option!);

    const body = await save();
    expect(body.steps[0].input).toMatchObject({
      domains: ["a.example.com"],
      key_algorithm: "ecdsa_p384",
    });
  });

  it("cert.apply 域名删到单个后 dns_zone 联动解除", async () => {
    posted.length = 0;
    const queryClient = renderPage();
    await waitForTasks(queryClient);
    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "p" } });

    await addStep("任务 B");
    await fillTwoDomains();

    // 删到只剩 1 个域名：必填联动解除，zone 清空也能提交。
    const remove = document.querySelector<HTMLElement>(".ant-select-selection-item-remove");
    expect(remove).toBeTruthy();
    fireEvent.click(remove!);
    await waitFor(() => {
      expect(
        document.querySelector("input#inputs_1_dns_zone[aria-required='true']"),
      ).toBeFalsy();
    });
    fireEvent.change(screen.getByLabelText("dns_zone"), { target: { value: "" } });
    fireEvent.click(screen.getByRole("button", { name: /保\s*存/ }));
    await waitFor(() => expect(posted).toHaveLength(1));
    expect(posted[0].steps[0].input).toMatchObject({ domains: ["b.example.com"] });
  });

  it("cert.apply 到期前 N 天随保存提交且与域名联动", async () => {
    posted.length = 0;
    const queryClient = renderPage();
    await waitForTasks(queryClient);
    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "p" } });
    await addStep("任务 B");
    const domains = await screen.findByLabelText("domains");
    fireEvent.change(domains, { target: { value: "example.com" } });
    fireEvent.keyDown(domains, { key: "Enter" });
    const renew = await screen.findByLabelText("renew_before_days");
    fireEvent.change(renew, { target: { value: "21" } });
    const body = await save();
    expect(body.steps[0].input).toMatchObject({ domains: ["example.com"], renew_before_days: 21 });
  });

  it("到期前检测已设置时可填 cron 一并创建调度（由后端透传验证，联动提示由集成用例覆盖）", async () => {
    posted.length = 0;
    const queryClient = renderPage();
    await waitForTasks(queryClient);
    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "p" } });
    await addStep("任务 B");
    const domains = await screen.findByLabelText("domains");
    fireEvent.change(domains, { target: { value: "example.com" } });
    fireEvent.keyDown(domains, { key: "Enter" });
    fireEvent.change(await screen.findByLabelText("renew_before_days"), { target: { value: "30" } });
    // 前端提示与 cron 联动由调度自动关联的后端链路覆盖，此处仅验到期前字段落保存体。
    const body = await save();
    expect(body.steps[0].input).toMatchObject({ renew_before_days: 30 });
  });
});