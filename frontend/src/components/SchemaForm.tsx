import { Col, Form, Input, InputNumber, Row, Select, Switch, Alert, Typography } from "antd";
import type { FormInstance } from "antd";
import type { Rule } from "antd/es/form";
import { useEffect, useMemo, useRef, useState } from "react";

/**
 * 由后端 JSON Schema（schemars 产物）驱动渲染 antd 表单（design D5）。
 *
 * 支持的字段类型：string / integer / number / boolean / enum / array<string>
 * / 一层嵌套 object；`array<object>` 与无属性的 `object` 以 JSON 文本编辑，
 * 表单 store 里存**解析后的结构**（见 {@link JsonTextArea}），未识别的结构降级为
 * JSON 文本编辑，不静默丢字段。
 * 解析 schemars 的 `$ref`（含 `allOf` 包裹）并把字符串枚举的 `oneOf` 扁平化成下拉；
 * 显隐联动遵循后端扩展：`x-visible-when: {"Equals": {field, values}}`、`x-hidden: bool`；
 * 另有调用方注入的 `x-options`，把「资源 ID」字段渲染成按名称选择的下拉。
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
  /** schemars 给「带说明的 $ref 字段」包的一层：`[{ "$ref": … }]`。 */
  allOf?: JsonSchemaNode[];
  /** schemars 对 Rust 枚举生成的分支；字符串枚举会被扁平化成 `enum`。 */
  oneOf?: JsonSchemaNode[];
  $defs?: Record<string, JsonSchemaNode>;
  definitions?: Record<string, JsonSchemaNode>;
}

interface Extension {
  "x-visible-when"?: Condition;
  "x-required-when"?: Condition;
  "x-hidden"?: boolean;
  /**
   * 下拉选项，由调用方注入（后端 schema 里没有）。用于「字段值是某个资源的 ID」
   * 的场景：把裸数字输入换成按名称选择，选项的 value 保持原始类型。
   */
  "x-options"?: Array<{ value: string | number; label: string }>;
}

type SchemaObject = JsonSchemaNode & Extension;

interface Condition {
  Equals?: { field: string; values: unknown[] };
}

export interface SchemaFormProps {
  /** 后端返回的 JSON Schema（schemars RootSchema 去壳后的对象部分）。 */
  schema: unknown;
  form: FormInstance;
  /** 嵌套前缀（Form.List 场景），如 ["steps", 0, "input"]。 */
  namePrefix?: (string | number)[];
  disabled?: boolean;
}

/**
 * 把 schemars 为字符串枚举生成的
 * `oneOf: [{enum:["dns-01"],type:"string"}, …]` 扁平化成 `enum: ["dns-01", …]`，
 * 前端才能渲染成下拉而不是自由文本。分支不是单值枚举时原样返回。
 */
function flattenEnum(node: JsonSchemaNode): JsonSchemaNode {
  const oneOf = node.oneOf;
  if (!Array.isArray(oneOf) || oneOf.length === 0) return node;

  const values: unknown[] = [];
  for (const option of oneOf) {
    if (!Array.isArray(option.enum) || option.enum.length !== 1) return node;
    values.push(option.enum[0]);
  }
  return { ...node, type: "string", enum: values };
}

/** 解引用 `$ref`（`#/definitions/…` 或 `#/$defs/…`），并展开枚举。 */
function deref(node: JsonSchemaNode, root: JsonSchemaNode): JsonSchemaNode {
  // 带说明的 $ref 字段会被包成 allOf：先解开内层，再用外层属性（description 等）覆盖。
  if (Array.isArray(node.allOf) && node.allOf.length === 1) {
    const merged: JsonSchemaNode = { ...deref(node.allOf[0], root), ...node };
    delete merged.allOf;
    delete merged.$ref;
    return flattenEnum(merged);
  }

  const ref = node.$ref;
  if (ref) {
    const name = ref.replace("#/$defs/", "").replace("#/definitions/", "");
    const defs = root.$defs ?? root.definitions;
    const target = defs?.[name];
    if (target) {
      const merged: JsonSchemaNode = { ...target, ...node };
      delete merged.$ref;
      return flattenEnum(merged);
    }
  }

  return flattenEnum(node);
}

/** 求值一条 `Equals` 条件；条件缺失或形态不认识时按「不满足」处理。 */
function conditionSatisfied(condition: Condition | undefined, values: Record<string, unknown>): boolean {
  const equals = condition?.Equals;
  if (!equals) return false;
  const actual = values[equals.field];
  return actual !== undefined && equals.values.some((value) => value === actual);
}

/** 读取一个字段的显隐标记；解析失败按「不隐藏」处理（宁多显示不误隐藏）。 */
function visibleWhen(node: SchemaObject, values: Record<string, unknown>): boolean {
  if (!node["x-visible-when"]?.Equals) return true;
  return conditionSatisfied(node["x-visible-when"], values);
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
        // 必填来源有二：字段本身在 required 列表里；或带了 x-required-when 且
        // 当前取值命中条件（如 directory_url 仅在 ca=custom 时必填）。
        const conditionallyRequired = conditionSatisfied(
          node["x-required-when"],
          values as Record<string, unknown>,
        );
        if (required.has(name) || conditionallyRequired) {
          // 报错文案不复用 description：那是帮助文本，红字里出现会让人以为
          // 「值不合法」，实际只是没填。description 仍作为 tooltip 展示。
          rules.push({ required: true, message: `请填写 ${name}` });
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
              <SchemaField node={node} form={form} path={path} root={root} disabled={disabled} />
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
  root,
  disabled,
  ...controlProps
}: {
  node: SchemaObject;
  form: FormInstance;
  path: (string | number)[];
  /** 根 schema，用于解析嵌套字段里的 `$ref`。 */
  root: JsonSchemaNode;
  disabled?: boolean;
} & Record<string, unknown>) {
  const type = Array.isArray(node.type) ? node.type[0] : node.type;

  // Form.Item 会把受控属性注入到它的直接子元素——这里是自定义组件而非具体控件，
  // 必须原样透传给真正的控件，否则控件是非受控的，用户输入进不了表单 store，
  // 提交时必填校验会失败（报错文案就是字段 description）。
  // 各控件 props 类型不同，故不做过度约束。
  const control = controlProps as any;

  // 调用方给了选项就当资源选择器用（如凭据 ID）：即使选项为空也渲染下拉，
  // 免得退化成裸数字输入后又不知道为什么填了不生效。
  const options = node["x-options"];
  if (Array.isArray(options)) {
    return (
      <Select
        disabled={disabled}
        placeholder={options.length === 0 ? "暂无可选项" : undefined}
        options={options.map((option) => ({ value: option.value, label: option.label }))}
        {...control}
      />
    );
  }

  if (node.enum && node.enum.length > 0) {
    return (
      <Select
        disabled={disabled}
        options={node.enum.map((value) => ({
          value: String(value),
          label: String(value),
        }))}
        {...control}
      />
    );
  }

  switch (type) {
    case "integer":
    case "number":
      return <InputNumber disabled={disabled} style={{ width: "100%" }} {...control} />;
    case "boolean":
      return <Switch disabled={disabled} {...control} />;
    case "array": {
      const itemType = Array.isArray(node.items?.type) ? node.items?.type[0] : node.items?.type;
      if (itemType === "object") {
        // 一层嵌套对象数组：以 JSON 文本编辑，store 里存解析后的数组。
        return (
          <JsonTextArea disabled={disabled} placeholder='[{"key": "value"}]' {...control} />
        );
      }
      return (
        <Select
          disabled={disabled}
          mode="tags"
          open={false}
          tokenSeparators={[","]}
          placeholder="输入后回车添加"
          {...control}
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
              const child = deref(rawNode, root) as SchemaObject;
              const childPath = [...path, name];
              return (
                <Col span={24} key={childPath.join(".")}>
                  <Form.Item
                    name={childPath}
                    label={name}
                    tooltip={child.description}
                    valuePropName={child.type === "boolean" ? "checked" : "value"}
                  >
                    <SchemaField
                      node={child}
                      form={form}
                      path={childPath}
                      root={root}
                      disabled={disabled}
                    />
                  </Form.Item>
                </Col>
              );
            })}
          </Row>
        );
      }
      return <JsonTextArea disabled={disabled} {...control} />;
    }
    case "string":
      return <Input disabled={disabled} {...control} />;
    default:
      // 没有可识别类型（如 `serde_json::Value`，结构随所选目标变化）：
      // 按 JSON 文本处理，别退化成单行文本框让人手打整个对象。
      return <JsonTextArea disabled={disabled} {...control} />;
  }
}

/**
 * JSON 文本编辑器：用于 `array<object>` 与无属性的 `object` 字段。
 *
 * 表单 store 里存的是**解析后的结构**，显示时才序列化成文本。这样做是
 * schema 驱动的：只有被渲染成 JSON 编辑器的字段会解析，`credentials`
 * 这类「值本身就是 JSON 字符串」的普通 string 字段不会被误解析。
 *
 * 输入暂时不是合法 JSON 时保留原文、标红提示，并**不覆盖**上一次有效值。
 * 失焦时若是合法 JSON 就把文本重排成 2 空格缩进的格式，方便阅读。
 */
function JsonTextArea({
  value,
  onChange,
  disabled,
  id,
  placeholder,
}: {
  value?: unknown;
  onChange?: (value: unknown) => void;
  disabled?: boolean;
  id?: string;
  placeholder?: string;
}) {
  const [text, setText] = useState(() => formatJson(value));
  const [invalid, setInvalid] = useState(false);
  // 自己发出的值回环成 value 时不要重置文本框，否则边打字边被重新格式化。
  const emitted = useRef<unknown>(undefined);

  useEffect(() => {
    if (value === emitted.current) return;
    emitted.current = value;
    setText(formatJson(value));
    setInvalid(false);
  }, [value]);

  const handleChange = (raw: string) => {
    setText(raw);
    if (raw.trim() === "") {
      emitted.current = undefined;
      setInvalid(false);
      onChange?.(undefined);
      return;
    }
    try {
      const parsed: unknown = JSON.parse(raw);
      emitted.current = parsed;
      setInvalid(false);
      onChange?.(parsed);
    } catch {
      // 非法 JSON：只标红，不把半成品写进表单（保留上一次有效值）。
      setInvalid(true);
    }
  };

  // 失焦时美化：合法 JSON 才重排（不打扰正在输入的半成品），非法则保留原文与标红。
  const handleBlur = () => {
    if (text.trim() === "") return;
    try {
      setText(formatJson(JSON.parse(text)));
      setInvalid(false);
    } catch {
      // 仍是非法 JSON，等用户改对再格式化。
    }
  };

  return (
    <Input.TextArea
      id={id}
      disabled={disabled}
      // 格式化后是多行 JSON，给足高度且随内容自适应；上限避免长内容吃掉整页。
      autoSize={{ minRows: 6, maxRows: 12 }}
      placeholder={placeholder}
      status={invalid ? "error" : undefined}
      value={text}
      onChange={(event) => handleChange(event.target.value)}
      onBlur={handleBlur}
    />
  );
}

/** 把结构序列化成可编辑文本；空值显示为空。 */
function formatJson(value: unknown): string {
  if (value === undefined || value === null) return "";
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

/** SchemaForm 内部使用：把 schema 的描述作为占位（预留扩展点）。 */
export function useSchemaPlaceholder(node: SchemaObject): string | undefined {
  return useMemo(() => node.description, [node.description]);
}
