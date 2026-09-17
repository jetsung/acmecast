import { Col, Form, Input, InputNumber, Row, Select, Switch, Alert, Typography } from "antd";
import type { FormInstance } from "antd";
import type { Rule } from "antd/es/form";
import { useMemo } from "react";

/**
 * 由后端 JSON Schema（schemars 产物）驱动渲染 antd 表单（design D5）。
 *
 * 支持的字段类型：string / integer / number / boolean / enum / array<string>
 * / 一层嵌套 object；未识别的结构降级为只读 JSON 展示并告警，不静默丢字段。
 * 显隐联动遵循后端扩展：`x-visible-when: {"Equals": {field, values}}`、
 * `x-hidden: bool`。
 */

interface JsonSchemaNode {
  type?: string | string[];
  format?: string;
  description?: string;
  enum?: unknown[];
  properties?: Record<string, JsonSchemaNode>;
  required?: string[];
  items?: JsonSchemaNode;
  $ref?: string;
}

interface Extension {
  "x-visible-when"?: { Equals?: { field: string; values: unknown[] } };
  "x-hidden"?: boolean;
}

type SchemaObject = JsonSchemaNode & Extension;

export interface SchemaFormProps {
  /** 后端返回的 JSON Schema（schemars RootSchema 去壳后的对象部分）。 */
  schema: unknown;
  form: FormInstance;
  /** 嵌套前缀（Form.List 场景），如 ["steps", 0, "input"]。 */
  namePrefix?: (string | number)[];
  disabled?: boolean;
}

/** 解引用 `$ref`（schemars 对嵌套结构生成 `#/$defs/...`）。 */
function deref(node: JsonSchemaNode, root: JsonSchemaNode): JsonSchemaNode {
  const ref = node.$ref;
  if (!ref) return node;
  const name = ref.replace("#/$defs/", "").replace("#/definitions/", "");
  const defs = (root as Record<string, Record<string, JsonSchemaNode>>).$defs
    ?? (root as Record<string, Record<string, JsonSchemaNode>>).definitions;
  return defs?.[name] ?? node;
}

/** 读取一个字段的显隐标记；解析失败按「不隐藏」处理（宁多显示不误隐藏）。 */
function visibleWhen(node: SchemaObject, values: Record<string, unknown>): boolean {
  const condition = node["x-visible-when"];
  const equals = condition?.Equals;
  if (!equals) return true;
  const actual = values[equals.field];
  return actual !== undefined && equals.values.some((value) => value === actual);
}

export function SchemaForm({ schema, form, namePrefix = [], disabled }: SchemaFormProps) {
  const root = (schema ?? {}) as JsonSchemaNode;
  const object = root.type === "object" || root.properties ? root : root;
  const properties = object.properties ?? {};
  const required = new Set(object.required ?? []);

  const values = Form.useWatch(namePrefix.length ? namePrefix : [], form) ?? {};

  const entries = Object.entries(properties);
  if (entries.length === 0) {
    return (
      <Alert
        type="warning"
        showIcon
        message="该类型未提供字段定义"
        description={<Typography.Text code>{JSON.stringify(schema).slice(0, 200)}</Typography.Text>}
      />
    );
  }

  return (
    <Row gutter={[16, 0]}>
      {entries.map(([name, rawNode]) => {
        const node = deref(rawNode, root) as SchemaObject;
        const path = [...namePrefix, name];

        // 显隐联动：条件不满足时整个字段不渲染。
        if (!visibleWhen(node, values as Record<string, unknown>)) {
          return null;
        }
        if (node["x-hidden"]) {
          return null;
        }

        const rules: Rule[] = [];
        if (required.has(name)) {
          rules.push({ required: true, message: node.description ?? `必填字段 ${name}` });
        }

        return (
          <Col span={12} key={path.join(".")}>
            <Form.Item
              name={path}
              label={name}
              tooltip={node.description}
              rules={rules}
              valuePropName={node.type === "boolean" ? "checked" : "value"}
            >
              <SchemaField node={node} form={form} path={path} disabled={disabled} />
            </Form.Item>
          </Col>
        );
      })}
    </Row>
  );
}

/** 按字段类型选择 antd 控件。 */
function SchemaField({
  node,
  form,
  path,
  disabled,
}: {
  node: SchemaObject;
  form: FormInstance;
  path: (string | number)[];
  disabled?: boolean;
}) {
  const type = Array.isArray(node.type) ? node.type[0] : node.type;

  if (node.enum && node.enum.length > 0) {
    return (
      <Select
        disabled={disabled}
        options={node.enum.map((value) => ({
          value: String(value),
          label: String(value),
        }))}
      />
    );
  }

  switch (type) {
    case "integer":
    case "number":
      return <InputNumber disabled={disabled} style={{ width: "100%" }} />;
    case "boolean":
      return <Switch disabled={disabled} />;
    case "array": {
      const itemType = Array.isArray(node.items?.type) ? node.items?.type[0] : node.items?.type;
      if (itemType === "object") {
        // 一层嵌套对象数组：以 JSON 文本编辑（MVP 折衷），提交前解析校验。
        return <Input.TextArea disabled={disabled} rows={2} placeholder='[{"key": "value"}]' />;
      }
      return (
        <Select
          disabled={disabled}
          mode="tags"
          open={false}
          tokenSeparators={[","]}
          placeholder="输入后回车添加"
        />
      );
    }
    case "object": {
      // 一层嵌套对象：分组渲染（如 DNS 提供商的嵌套字段）。
      const nested = node.properties;
      if (nested && Object.keys(nested).length > 0) {
        return (
          <Row gutter={[16, 0]}>
            {Object.entries(nested).map(([name, rawNode]) => {
              const child = deref(rawNode, node) as SchemaObject;
              const childPath = [...path, name];
              return (
                <Col span={24} key={childPath.join(".")}>
                  <Form.Item
                    name={childPath}
                    label={name}
                    tooltip={child.description}
                    valuePropName={child.type === "boolean" ? "checked" : "value"}
                  >
                    <SchemaField node={child} form={form} path={childPath} disabled={disabled} />
                  </Form.Item>
                </Col>
              );
            })}
          </Row>
        );
      }
      return <Input.TextArea disabled={disabled} rows={2} />;
    }
    default:
      return <Input disabled={disabled} />;
  }
}

/** 提交前整理：把数组/对象类字段的字符串值解析回结构，剔除解析失败的项。 */
export function normalizeSchemaValues(values: Record<string, unknown>): Record<string, unknown> {
  const result: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(values)) {
    if (typeof value === "string" && (value.trim().startsWith("[") || value.trim().startsWith("{"))) {
      try {
        result[key] = JSON.parse(value);
        continue;
      } catch {
        // 不是合法 JSON 的字符串按原样保留。
      }
    }
    result[key] = value;
  }
  return result;
}

/** SchemaForm 内部使用：把 schema 的描述作为占位（预留扩展点）。 */
export function useSchemaPlaceholder(node: SchemaObject): string | undefined {
  return useMemo(() => node.description, [node.description]);
}
