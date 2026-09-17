/**
 * JSON 编辑器高度：验证渲染器把 `minRows=6 / maxRows=12` 的 autoSize 配置
 * 真正传给了 Input.TextArea。
 *
 * 为什么不直接量像素：jsdom 没有布局，antd 的 autoSize 只能算出固定的
 * 32px 行内高度——minRows=3 与 minRows=6 渲染结果完全一样，测不出效果。
 * 所以退一步捕获真正下发的 prop，并顺带确认用的是 autoSize（无 rows 属性）
 * 而不是固定行数。
 */
import { render, screen } from "@testing-library/react";
import { Form } from "antd";
import type { ComponentProps } from "react";
import { describe, expect, it, vi } from "vitest";
import { SchemaForm } from "./SchemaForm";

const captured = vi.hoisted(() => [] as unknown[]);

vi.mock("antd", async (importOriginal) => {
  const actual = await importOriginal<typeof import("antd")>();
  const OriginalTextArea = actual.Input.TextArea;
  const TextArea = (props: ComponentProps<typeof OriginalTextArea>) => {
    captured.push(props.autoSize);
    return <OriginalTextArea {...props} />;
  };
  return { ...actual, Input: Object.assign(actual.Input, { TextArea }) };
});

const schema = {
  type: "object",
  properties: { rules: { type: "array", items: { type: "object" } } },
};

function Harness() {
  const [form] = Form.useForm();
  return (
    <Form form={form}>
      <SchemaForm schema={schema} form={form} namePrefix={["fields"]} />
    </Form>
  );
}

describe("JSON 编辑器高度", () => {
  it("以 minRows=6 / maxRows=12 自适应高度，而非固定 rows", () => {
    captured.length = 0;
    render(<Harness />);

    expect(captured).toContainEqual({ minRows: 6, maxRows: 12 });

    const area = screen.getByRole<HTMLTextAreaElement>("textbox");
    // autoSize 模式不会写 rows 属性，而是写出行内高度约束。
    expect(area.getAttribute("rows")).toBeNull();
    expect(area.getAttribute("style")).toContain("min-height");
  });
});
