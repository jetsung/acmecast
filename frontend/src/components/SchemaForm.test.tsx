/**
 * SchemaForm：Schema 驱动的字段值必须真正进入表单 store，且各控件类型都要绑定。
 *
 * 回归用例：`Form.Item` 的直接子元素是自定义组件 `SchemaField`，而非具体控件。
 * antd 会把受控属性注入到这个直接子元素上；若 `SchemaField` 不透传，控件就是非
 * 受控的——用户输入进不了 store，提交时必填校验失败，报错文案还是字段的
 * `description`（看起来像「填了值却报说明文字」）。
 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { Form } from "antd";
import { describe, expect, it, vi } from "vitest";
import { SchemaForm } from "./SchemaForm";

/** 造一个对象 Schema，只关注 properties/required。 */
function objectSchema(
  properties: Record<string, unknown>,
  required: string[] = [],
): Record<string, unknown> {
  return { type: "object", required, properties };
}

function Harness({ schema, onFinish }: { schema: unknown; onFinish: (values: unknown) => void }) {
  const [form] = Form.useForm();
  return (
    <Form form={form} onFinish={onFinish}>
      <SchemaForm schema={schema} form={form} namePrefix={["fields"]} />
      <button type="submit">确定</button>
    </Form>
  );
}

/** 点「确定」并断言 onFinish 收到值；未提交（校验失败）会在 waitFor 超时。 */
async function submitAndGet(onFinish: ReturnType<typeof vi.fn>): Promise<Record<string, unknown>> {
  fireEvent.click(screen.getByRole("button", { name: "确定" }));
  await waitFor(() => expect(onFinish).toHaveBeenCalledTimes(1));
  return onFinish.mock.calls[0][0] as Record<string, unknown>;
}

describe("SchemaForm", () => {
  it("x-full-width：标注的标量字段占整行，未标注的保持半宽", () => {
    render(
      <Harness
        onFinish={vi.fn()}
        schema={objectSchema({
          directory_url: {
            type: ["string", "null"],
            description: "自定义 Directory URL。",
            "x-full-width": true,
          },
          eab_kid: { type: ["string", "null"], description: "EAB 密钥标识。" },
        })}
      />,
    );

    // 宽度按 antd 的 Col span 落到 class 上：整行 24，半宽 12。
    const cols = Array.from(document.querySelectorAll<HTMLElement>(".ant-form-item")).map(
      (item) => item.closest(".ant-col")?.className ?? "",
    );
    expect(cols).toHaveLength(2);
    expect(cols[0]).toContain("ant-col-24");
    expect(cols[1]).toContain("ant-col-12");
  });

  it("x-end-row：半宽字段渲染后补空占位列，后续字段从新行开始", () => {
    render(
      <Harness
        onFinish={vi.fn()}
        schema={objectSchema({
          account_site: { type: "string", enum: ["cn", "intl"], "x-end-row": true },
          secret_id: { type: "string" },
          secret_key: { type: "string" },
        })}
      />,
    );

    // 三个字段都是半宽（12）；account_site 行尾多一个 aria-hidden 空列
    // 占位——独占一行但右侧留空，密钥对从第二行开始。
    const cols = Array.from(document.querySelectorAll<HTMLElement>(".ant-form-item")).map(
      (item) => item.closest(".ant-col")?.className ?? "",
    );
    expect(cols).toHaveLength(3);
    for (const className of cols) {
      expect(className).toContain("ant-col-12");
    }
    const spacers = document.querySelectorAll<HTMLElement>(".ant-col[aria-hidden]");
    expect(spacers).toHaveLength(1);
    expect(spacers[0].className).toContain("ant-col-12");
  });

  it("x-multiline：标注的字段占整行（既有机制不回归）", () => {
    render(
      <Harness
        onFinish={vi.fn()}
        schema={objectSchema({
          credentials: { type: ["string", "null"], "x-multiline": true },
        })}
      />,
    );

    const col = document
      .querySelector<HTMLElement>(".ant-form-item")
      ?.closest(".ant-col")?.className;
    expect(col).toContain("ant-col-24");
  });

  it("string：把输入提交到 namePrefix 指定的路径下", async () => {
    const onFinish = vi.fn();
    render(
      <Harness
        onFinish={onFinish}
        schema={objectSchema(
          {
            ca: { type: "string", description: "CA 类型：内置别名（letsencrypt、…）或 custom。" },
          },
          ["ca"],
        )}
      />,
    );

    fireEvent.change(screen.getByRole("textbox"), { target: { value: "letsencrypt" } });

    expect(await submitAndGet(onFinish)).toEqual({ fields: { ca: "letsencrypt" } });
  });

  it("必填字段为空时给出必填提示，而不是复用字段说明", async () => {
    // 回归：曾经把 description 直接当 required 的报错文案，导致「明明是没填，
    // 却显示一段说明文字」，看起来像「填了值还报错」。
    const onFinish = vi.fn();
    render(
      <Harness
        onFinish={onFinish}
        schema={objectSchema(
          {
            ca: {
              type: "string",
              description: "CA 类型：内置别名（letsencrypt、…）或 custom。",
            },
          },
          ["ca"],
        )}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "确定" }));

    expect(await screen.findByText("请填写 ca")).toBeTruthy();
    expect(screen.queryByText(/内置别名/)).toBeNull();
    expect(onFinish).not.toHaveBeenCalled();
  });

  it("x-required-when：条件命中时字段转必填，不命中时可留空", async () => {
    // acme.account 的诉求：ca=custom 时 directory_url 必填；内置 CA 上它是
    // 可选的端点覆盖项。条件由后端 schema 以 x-required-when 扩展下发。
    const onFinish = vi.fn();
    render(
      <Harness
        onFinish={onFinish}
        schema={objectSchema(
          {
            ca: { type: "string", enum: ["letsencrypt", "custom"] },
            directory_url: {
              type: "string",
              "x-required-when": { Equals: { field: "ca", values: ["custom"] } },
            },
          },
          ["ca"],
        )}
      />,
    );

    // 内置 CA：留空也能提交。
    fireEvent.mouseDown(screen.getByRole("combobox"));
    fireEvent.click(await screen.findByTitle("letsencrypt"));
    expect(await submitAndGet(onFinish)).toEqual({ fields: { ca: "letsencrypt" } });

    // 切到 custom：同一字段转必填，空值提交被拦下。
    onFinish.mockClear();
    fireEvent.mouseDown(screen.getByRole("combobox"));
    fireEvent.click(await screen.findByTitle("custom"));
    // 等联动重渲染落地（label 出现必填星号）再提交，避免与规则更新竞态。
    await waitFor(() => {
      expect(
        document.querySelector("#fields_directory_url[aria-required='true']"),
      ).toBeTruthy();
    });
    fireEvent.click(screen.getByRole("button", { name: "确定" }));
    expect(await screen.findByText("请填写 directory_url")).toBeTruthy();
    expect(onFinish).not.toHaveBeenCalled();

    // 补上值后即可提交。
    onFinish.mockClear();
    fireEvent.change(screen.getByLabelText("directory_url"), {
      target: { value: "https://ca.internal/dir" },
    });
    expect(await submitAndGet(onFinish)).toEqual({
      fields: { ca: "custom", directory_url: "https://ca.internal/dir" },
    });
  });

  it("x-required-when MinItems：数组字段达标时转必填，不达标时可留空", async () => {
    // cert.apply 的诉求：domains 配置多个域名时 dns_zone 必填（多域名下
    // 静默推导 zone 容易踩错），单域名时保持可选。条件由后端 schema 以
    // x-required-when 扩展下发，这里是求值器的行为契约。
    const onFinish = vi.fn();
    render(
      <Harness
        onFinish={onFinish}
        schema={objectSchema({
          domains: { type: "array", items: { type: "string" } },
          dns_zone: {
            type: ["string", "null"],
            description: "DNS zone；多个域名时必须显式配置。",
            "x-required-when": { MinItems: { field: "domains", count: 2 } },
          },
        })}
      />,
    );

    const combo = screen.getByRole("combobox");
    // 单域名：dns_zone 不必填，留空也能提交。
    fireEvent.change(combo, { target: { value: "a.example.com" } });
    fireEvent.keyDown(combo, { key: "Enter" });
    expect(await submitAndGet(onFinish)).toEqual({ fields: { domains: ["a.example.com"] } });

    // 第二个域名：同一字段转必填，空值提交被拦下。
    onFinish.mockClear();
    fireEvent.change(combo, { target: { value: "b.example.com" } });
    fireEvent.keyDown(combo, { key: "Enter" });
    // 等联动重渲染落地（label 出现必填星号）再提交，避免与规则更新竞态。
    await waitFor(() => {
      expect(document.querySelector("#fields_dns_zone[aria-required='true']")).toBeTruthy();
    });
    fireEvent.click(screen.getByRole("button", { name: "确定" }));
    expect(await screen.findByText("请填写 dns_zone")).toBeTruthy();
    expect(onFinish).not.toHaveBeenCalled();

    // 补上 zone 后即可提交。
    onFinish.mockClear();
    fireEvent.change(screen.getByLabelText("dns_zone"), {
      target: { value: "example.com" },
    });
    expect(await submitAndGet(onFinish)).toEqual({
      fields: { domains: ["a.example.com", "b.example.com"], dns_zone: "example.com" },
    });
  });

  it("x-required-when MinItems：目标字段不是数组（或缺失）时按不满足处理", async () => {
    // 防误伤：MinItems 只对数组字段生效，非数组/缺失一律不算命中，
    // 不会把字段误标成必填。
    const onFinish = vi.fn();
    render(
      <Harness
        onFinish={onFinish}
        schema={objectSchema({
          domains: { type: "string" },
          dns_zone: {
            type: "string",
            "x-required-when": { MinItems: { field: "domains", count: 2 } },
          },
        })}
      />,
    );

    fireEvent.change(screen.getByLabelText("domains"), {
      target: { value: "a.example.com" },
    });

    expect(await submitAndGet(onFinish)).toEqual({
      fields: { domains: "a.example.com" },
    });
  });

  it("x-visible-when：Equals 控制显隐，不受 MinItems 分支影响", async () => {
    const onFinish = vi.fn();
    render(
      <Harness
        onFinish={onFinish}
        schema={objectSchema({
          challenge: { type: "string", enum: ["dns-01", "http-01"] },
          dns_provider: {
            type: "string",
            "x-visible-when": { Equals: { field: "challenge", values: ["dns-01"] } },
          },
        })}
      />,
    );

    // dns-01：字段显示。
    fireEvent.mouseDown(screen.getByRole("combobox"));
    fireEvent.click(await screen.findByTitle("dns-01"));
    expect(await screen.findByLabelText("dns_provider")).toBeTruthy();

    // 切到 http-01：字段不再渲染，提交不受影响。
    fireEvent.mouseDown(screen.getByRole("combobox"));
    fireEvent.click(await screen.findByTitle("http-01"));
    await waitFor(() => {
      expect(screen.queryByLabelText("dns_provider")).toBeNull();
    });
    expect(await submitAndGet(onFinish)).toEqual({ fields: { challenge: "http-01" } });
  });

  it("boolean：Switch 的 checked 能提交", async () => {
    const onFinish = vi.fn();
    render(
      <Harness
        onFinish={onFinish}
        schema={objectSchema({ force: { type: "boolean" } }, ["force"])}
      />,
    );

    fireEvent.click(screen.getByRole("switch"));

    expect(await submitAndGet(onFinish)).toEqual({ fields: { force: true } });
  });

  it("number：InputNumber 的数值能提交", async () => {
    const onFinish = vi.fn();
    render(
      <Harness
        onFinish={onFinish}
        schema={objectSchema({ retries: { type: "integer" } }, ["retries"])}
      />,
    );

    fireEvent.change(screen.getByRole("spinbutton"), { target: { value: "3" } });

    expect(await submitAndGet(onFinish)).toEqual({ fields: { retries: 3 } });
  });

  it("enum：Select 选中的值能提交", async () => {
    const onFinish = vi.fn();
    render(
      <Harness
        onFinish={onFinish}
        schema={objectSchema({ mode: { type: "string", enum: ["normal", "force"] } }, ["mode"])}
      />,
    );

    // antd v6 的 Select 会渲染一份隐藏的无障碍 option 列表，真正可点的是带 title
    // 的可见项（.ant-select-item-option），所以按 title 取。
    fireEvent.mouseDown(screen.getByRole("combobox"));
    fireEvent.click(await screen.findByTitle("force"));

    expect(await submitAndGet(onFinish)).toEqual({ fields: { mode: "force" } });
  });

  it("enum：$ref + allOf 包装的枚举（schemars 产物）渲染成下拉", async () => {
    const onFinish = vi.fn();
    render(
      <Harness
        onFinish={onFinish}
        schema={{
          type: "object",
          required: ["challenge"],
          definitions: {
            ChallengeKind: {
              oneOf: [
                { enum: ["dns-01"], type: "string" },
                { enum: ["http-01"], type: "string" },
              ],
            },
          },
          properties: {
            challenge: {
              allOf: [{ $ref: "#/definitions/ChallengeKind" }],
              description: "挑战类型。",
            },
          },
        }}
      />,
    );

    // 关键：渲染成下拉（combobox）而不是一张自由文本框。
    fireEvent.mouseDown(screen.getByRole("combobox"));
    fireEvent.click(await screen.findByTitle("http-01"));

    expect(await submitAndGet(onFinish)).toEqual({ fields: { challenge: "http-01" } });
  });

  it("x-options：按调用方给的选项渲染下拉，提交原始（数字）值", async () => {
    const onFinish = vi.fn();
    render(
      <Harness
        onFinish={onFinish}
        schema={objectSchema(
          {
            account_credential_id: {
              type: "integer",
              description: "ACME 账号凭据标识。",
              "x-options": [
                { value: 1, label: "le-prod（id=1）" },
                { value: 2, label: "le-staging（id=2）" },
              ],
            },
          },
          ["account_credential_id"],
        )}
      />,
    );

    fireEvent.mouseDown(screen.getByRole("combobox"));
    fireEvent.click(await screen.findByTitle("le-staging（id=2）"));

    // 值保持数字类型，不是字符串 "2"。
    expect(await submitAndGet(onFinish)).toEqual({ fields: { account_credential_id: 2 } });
  });

  it("x-options：没有可选资源时也渲染下拉并提示", () => {
    render(
      <Harness
        onFinish={vi.fn()}
        schema={objectSchema(
          { account_credential_id: { type: "integer", "x-options": [] } },
          ["account_credential_id"],
        )}
      />,
    );

    expect(screen.getByRole("combobox")).toBeTruthy();
    expect(screen.getByText("暂无可选项")).toBeTruthy();
  });

  it("无类型字段（如 serde_json::Value）按 JSON 文本处理，而不是单行输入", async () => {
    const onFinish = vi.fn();
    render(
      <Harness
        onFinish={onFinish}
        schema={objectSchema({ config: { description: "结构与所选目标一致。" } })}
      />,
    );

    const area = screen.getByRole("textbox");
    expect(area.tagName).toBe("TEXTAREA");
    fireEvent.change(area, { target: { value: '{"cert_path":"/etc/a.crt"}' } });

    expect(await submitAndGet(onFinish)).toEqual({
      fields: { config: { cert_path: "/etc/a.crt" } },
    });
  });

  /** 模拟后端注入的 cert.deploy config 扩展：local 必填两个路径；ssh 含可空的 auth 枚举。 */
  function targetSchemas(): Record<
    string,
    { display_name: string; example: unknown; schema: unknown }
  > {
    return {
      local: {
        display_name: "本地文件系统",
        example: { cert_path: "/etc/a.crt", key_path: "/etc/a.key" },
        schema: {
          type: "object",
          required: ["cert_path", "key_path"],
          properties: {
            cert_path: { type: "string", description: "证书链写入路径。" },
            key_path: { type: "string", description: "私钥写入路径。" },
          },
        },
      },
      ssh: {
        display_name: "SSH 远程主机",
        example: { host: "10.0.0.1" },
        schema: {
          type: "object",
          properties: {
            host: { type: ["string", "null"], description: "目标主机。" },
            auth: {
              description: "认证方式。",
              anyOf: [{ $ref: "#/definitions/SshAuth" }, { type: "null" }],
            },
          },
          definitions: {
            SshAuth: {
              oneOf: [
                {
                  type: "object",
                  required: ["credential_id", "kind"],
                  properties: {
                    credential_id: { type: "integer", description: "私钥凭据的标识。" },
                    kind: { type: "string", enum: ["private_key"] },
                  },
                },
                {
                  type: "object",
                  required: ["credential_id", "kind"],
                  properties: {
                    credential_id: { type: "integer", description: "口令凭据的标识。" },
                    kind: { type: "string", enum: ["password"] },
                  },
                },
              ],
            },
          },
        },
      },
    };
  }

  function deploySchema(): Record<string, unknown> {
    return objectSchema({
      target: { type: "string" },
      config: { description: "目标的输入配置。", "x-target-schemas": targetSchemas() },
    });
  }

  it("x-target-schemas：未选 target 时提示先选目标（只读占位）", () => {
    render(<Harness onFinish={vi.fn()} schema={deploySchema()} />);

    const hint = screen.getByPlaceholderText<HTMLInputElement>(
      "先选择 target（local：本地文件系统；ssh：SSH 远程主机）",
    );
    expect(hint.tagName).toBe("INPUT");
    expect(hint.disabled).toBe(true);
  });

  it("x-target-schemas：local 变体渲染成子表单，required 下推，提交结构化 config", async () => {
    const onFinish = vi.fn();
    render(<Harness onFinish={onFinish} schema={deploySchema()} />);

    // target 在前、config 占位在后：textbox[0] 是 target 的单行输入。
    fireEvent.change(screen.getAllByRole("textbox")[0], { target: { value: "local" } });
    // 先等子表单挂载完（否则提交时校验不到这些字段）。
    const certPath = await screen.findByLabelText("cert_path");

    // 变体 schema 的 required（cert_path/key_path）下推成表单必填。
    fireEvent.click(screen.getByRole("button", { name: "确定" }));
    expect(await screen.findByText("请填写 cert_path")).toBeTruthy();

    fireEvent.change(certPath, { target: { value: "/etc/a.crt" } });
    fireEvent.change(await screen.findByLabelText("key_path"), {
      target: { value: "/etc/a.key" },
    });

    expect(await submitAndGet(onFinish)).toEqual({
      fields: {
        target: "local",
        config: { cert_path: "/etc/a.crt", key_path: "/etc/a.key" },
      },
    });
  });

  it("x-target-schemas：切换 target 清空已填字段，避免残留进新目标", async () => {
    const onFinish = vi.fn();
    render(<Harness onFinish={onFinish} schema={deploySchema()} />);

    const targetInput = screen.getAllByRole("textbox")[0];
    fireEvent.change(targetInput, { target: { value: "local" } });
    fireEvent.change(await screen.findByLabelText("cert_path"), {
      target: { value: "/etc/a.crt" },
    });

    fireEvent.change(targetInput, { target: { value: "ssh" } });
    await screen.findByLabelText("host");

    fireEvent.change(targetInput, { target: { value: "local" } });
    const certPath = await screen.findByLabelText<HTMLInputElement>("cert_path");
    expect(certPath.value).toBe("");
  });

  it("x-target-schemas：ssh 变体的 auth（可空 tagged enum）合并渲染成 kind 下拉与凭据输入", async () => {
    const onFinish = vi.fn();
    render(<Harness onFinish={onFinish} schema={deploySchema()} />);

    fireEvent.change(screen.getAllByRole("textbox")[0], { target: { value: "ssh" } });

    // auth 的 oneOf 每支是对象（internally tagged）：合并后 kind 是双值下拉、
    // credential_id 是数字输入，提交结构与 serde 的 tag 写法一致。
    fireEvent.mouseDown(await screen.findByRole("combobox"));
    fireEvent.click(await screen.findByTitle("private_key"));
    fireEvent.change(await screen.findByRole("spinbutton"), { target: { value: "7" } });

    expect(await submitAndGet(onFinish)).toEqual({
      fields: {
        target: "ssh",
        config: { auth: { kind: "private_key", credential_id: 7 } },
      },
    });
  });

  it("enum：顶层 $ref 的字符串枚举同样扁平化成下拉", async () => {
    const onFinish = vi.fn();
    render(
      <Harness
        onFinish={onFinish}
        schema={{
          type: "object",
          required: ["mode"],
          definitions: {
            Mode: {
              oneOf: [
                { enum: ["normal"], type: "string" },
                { enum: ["force"], type: "string" },
              ],
            },
          },
          properties: { mode: { $ref: "#/definitions/Mode" } },
        }}
      />,
    );

    fireEvent.mouseDown(screen.getByRole("combobox"));
    fireEvent.click(await screen.findByTitle("force"));

    expect(await submitAndGet(onFinish)).toEqual({ fields: { mode: "force" } });
  });

  it("object：嵌套字段按子路径提交", async () => {
    const onFinish = vi.fn();
    render(
      <Harness
        onFinish={onFinish}
        schema={objectSchema({
          target: objectSchema({ host: { type: "string" } }, ["host"]),
        })}
      />,
    );

    fireEvent.change(await screen.findByLabelText("host"), { target: { value: "1.2.3.4" } });

    expect(await submitAndGet(onFinish)).toEqual({ fields: { target: { host: "1.2.3.4" } } });
  });

  it("array<string>：tags Select 输入的多个值以字符串数组提交", async () => {
    const onFinish = vi.fn();
    render(
      <Harness
        onFinish={onFinish}
        schema={objectSchema({ tags: { type: "array", items: { type: "string" } } })}
      />,
    );

    const combo = screen.getByRole("combobox");
    for (const tag of ["one", "two"]) {
      fireEvent.change(combo, { target: { value: tag } });
      fireEvent.keyDown(combo, { key: "Enter" });
    }

    expect(await submitAndGet(onFinish)).toEqual({ fields: { tags: ["one", "two"] } });
  });

  it("array<object>：JSON 文本编辑器把输入解析成结构后提交", async () => {
    const onFinish = vi.fn();
    render(
      <Harness
        onFinish={onFinish}
        schema={objectSchema({ rules: { type: "array", items: { type: "object" } } })}
      />,
    );

    const area = screen.getByRole("textbox");
    expect(area.tagName).toBe("TEXTAREA");
    fireEvent.change(area, { target: { value: '[{"key":"value"}]' } });

    expect(await submitAndGet(onFinish)).toEqual({ fields: { rules: [{ key: "value" }] } });
  });

  it("array<object>：非法 JSON 标红且不覆盖上一次有效值", async () => {
    const onFinish = vi.fn();
    render(
      <Harness
        onFinish={onFinish}
        schema={objectSchema({ rules: { type: "array", items: { type: "object" } } })}
      />,
    );

    const area = screen.getByRole<HTMLTextAreaElement>("textbox");
    fireEvent.change(area, { target: { value: '[{"key":"ok"}]' } });
    // 打到一半的非法 JSON：应标红，但表单里仍是上一次解析成功的值。
    fireEvent.change(area, { target: { value: '[{"key":' } });
    expect(area.className).toContain("status-error");

    expect(await submitAndGet(onFinish)).toEqual({ fields: { rules: [{ key: "ok" }] } });
  });

  it("array<object>：失焦把 minified JSON 重排为缩进格式", async () => {
    const onFinish = vi.fn();
    render(
      <Harness
        onFinish={onFinish}
        schema={objectSchema({ rules: { type: "array", items: { type: "object" } } })}
      />,
    );

    const area = screen.getByRole<HTMLTextAreaElement>("textbox");
    fireEvent.change(area, { target: { value: '[{"key":"v"}]' } });
    fireEvent.blur(area);

    expect(area.value).toBe('[\n  {\n    "key": "v"\n  }\n]');
    // 格式化只改显示，提交的仍是解析后的结构。
    expect(await submitAndGet(onFinish)).toEqual({ fields: { rules: [{ key: "v" }] } });
  });

  it("array<object>：非法 JSON 失焦不重排，保留原文", async () => {
    const onFinish = vi.fn();
    render(
      <Harness
        onFinish={onFinish}
        schema={objectSchema({ rules: { type: "array", items: { type: "object" } } })}
      />,
    );

    const area = screen.getByRole<HTMLTextAreaElement>("textbox");
    fireEvent.change(area, { target: { value: '[{"key":' } });
    fireEvent.blur(area);

    expect(area.value).toBe('[{"key":');
  });

  it("string：值形如 JSON 也不会被误解析（schema 驱动）", async () => {
    const onFinish = vi.fn();
    render(
      <Harness
        onFinish={onFinish}
        schema={objectSchema({ credentials: { type: "string" } })}
      />,
    );

    const raw = '{"id":"acct/1","key_pkcs8":"AAA"}';
    fireEvent.change(screen.getByRole("textbox"), { target: { value: raw } });

    expect(await submitAndGet(onFinish)).toEqual({ fields: { credentials: raw } });
  });

  it("x-multiline：string 字段渲染成文本域，PEM 换行原样提交", async () => {
    // SSH 档案的私钥是多行 PEM：单行 Input 会吞掉换行或让粘贴难用，
    // 这里钉住「文本域 + 逐字节原样提交」的行为。
    const onFinish = vi.fn();
    render(
      <Harness
        onFinish={onFinish}
        schema={objectSchema(
          { private_key: { type: "string", "x-multiline": true } },
          ["private_key"],
        )}
      />,
    );

    const area = screen.getByRole<HTMLTextAreaElement>("textbox");
    expect(area.tagName).toBe("TEXTAREA");

    const pem = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk\n-----END OPENSSH PRIVATE KEY-----";
    fireEvent.change(area, { target: { value: pem } });

    expect(await submitAndGet(onFinish)).toEqual({ fields: { private_key: pem } });
  });

  it("无 x-multiline 的 string 仍是单行输入", () => {
    render(
      <Harness
        onFinish={vi.fn()}
        schema={objectSchema({ host: { type: "string" } })}
      />,
    );

    expect(screen.getByRole("textbox").tagName).toBe("INPUT");
  });

  it("default：schema 缺省值预填进表单并随提交返回", async () => {
    // 对应 SSH 主机档案：user=root、cert_mode=0644 由 schema default 预填。
    const onFinish = vi.fn();
    render(
      <Harness
        onFinish={onFinish}
        schema={objectSchema({
          host: { type: "string" },
          user: { type: "string", default: "root", description: "登录用户。" },
          cert_mode: {
            type: ["string", "null"],
            default: "0644",
            description: "证书权限。",
          },
        })}
      />,
    );

    expect(await screen.findByDisplayValue("root")).toBeTruthy();
    expect(screen.getByDisplayValue("0644")).toBeTruthy();

    fireEvent.change(screen.getByLabelText("host"), { target: { value: "web-1" } });

    expect(await submitAndGet(onFinish)).toEqual({
      fields: { host: "web-1", user: "root", cert_mode: "0644" },
    });
  });

  it("default：预填值可被用户改写，编辑回填的已有值不被覆盖", async () => {
    const onFinish = vi.fn();
    render(
      <Harness
        onFinish={onFinish}
        schema={objectSchema({
          user: { type: "string", default: "root" },
        })}
      />,
    );

    const input = await screen.findByDisplayValue<HTMLInputElement>("root");
    fireEvent.change(input, { target: { value: "deploy" } });

    expect(await submitAndGet(onFinish)).toEqual({ fields: { user: "deploy" } });
  });

  it("ssh 凭据 schema：不渲染部署输入的路径与重载命令字段", () => {
    // 模拟凭据类型 schema 若被部署输入 schema 污染时的回归哨兵：
    // 档案只含连接信息与权限，cert_path/key_path/reload_command 属部署输入。
    render(
      <Harness
        onFinish={vi.fn()}
        schema={objectSchema({
          host: { type: "string" },
          user: { type: "string", default: "root" },
          cert_mode: { type: ["string", "null"], default: "0644" },
          key_mode: { type: ["string", "null"], default: "0600" },
        })}
      />,
    );

    expect(screen.getByLabelText("host")).toBeTruthy();
    for (const banned of ["cert_path", "key_path", "reload_command"]) {
      expect(screen.queryByLabelText(banned)).toBeNull();
    }
  });

  it("x-field-order：按声明序渲染，未声明的字段按原序追加在后", async () => {
    // cert.deploy SSH 变体的诉求：成对字段相邻（host/port、cert_path/cert_mode、
    // key_path/key_mode），顺序由前端显式声明，不依赖后端 schema 键序。
    const onFinish = vi.fn();
    render(
      <Harness
        onFinish={onFinish}
        schema={{
          type: "object",
          properties: {
            reload_command: { type: "string" },
            key_mode: { type: "string" },
            cert_mode: { type: "string" },
            key_path: { type: "string" },
            cert_path: { type: "string" },
            port: { type: "integer" },
            host: { type: "string" },
          },
          "x-field-order": ["host", "port", "cert_path", "cert_mode", "key_path", "key_mode"],
        } as unknown as Parameters<typeof SchemaForm>[0]["schema"]}
      />,
    );

    // 声明序渲染：成对字段相邻；未声明的 reload_command 追加在最后。
    const labels = ["host", "port", "cert_path", "cert_mode", "key_path", "key_mode", "reload_command"];
    const formItems = Array.from(document.querySelectorAll<HTMLElement>(".ant-form-item"));
    const order = formItems
      .map((item) => item.querySelector("label")?.textContent)
      .filter((text): text is string => text !== null);
    expect(order).toEqual(labels);

    fireEvent.change(screen.getByLabelText("host"), { target: { value: "web-1" } });
    // 未填字段不进 store,提交值只有实际填写的字段——排序不改变提交语义。
    expect(await submitAndGet(onFinish)).toEqual({ fields: { host: "web-1" } });
  });
});
