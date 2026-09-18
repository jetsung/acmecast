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
});
