import { Col, Form, Input, InputNumber, Row, Select, Switch, Alert, Typography } from "antd";
import type { FormInstance } from "antd";
import type { Rule } from "antd/es/form";
import { Fragment, useEffect, useMemo, useRef, useState } from "react";

/**
 * 由后端 JSON Schema（schemars 产物）驱动渲染 antd 表单（design D5）。
 *
 * 支持的字段类型：string / integer / number / boolean / enum / array<string>
 * / 一层嵌套 object；`array<object>` 与无属性的 `object` 以 JSON 文本编辑，
 * 表单 store 里存**解析后的结构**（见 {@link JsonTextArea}），未识别的结构降级为
 * JSON 文本编辑，不静默丢字段。
 * 解析 schemars 的 `$ref`（含 `allOf` 包裹）并把字符串枚举的 `oneOf` 扁平化成下拉；
 * `Option<T>`（`anyOf: [T, null]`）解包出内层并标记可空；internally tagged 枚举
 * （oneOf 每支是对象，如 SSH 认证的 `kind`）合并成普通对象渲染成表单；
 * 显隐联动遵循后端扩展：`x-visible-when: {"Equals": {field, values}}`、`x-hidden: bool`；
 * 必填联动同理（`x-required-when`，支持 `Equals` 与数组长度下限 `MinItems`，
 * 后者用于 cert.apply 的「多域名时 dns_zone 必填」）；
 * 另有调用方注入的 `x-options`，把「资源 ID」字段渲染成按名称选择的下拉；
 * `x-multiline`（后端 schema 注入，如 SSH 档案的私钥）：string 字段渲染成多行文本域；
 * `x-target-schemas`（cert.deploy 的 config）：按同级 `target` 的取值渲染成
 * 所选部署目标的**结构化子表单**，未选 target 时提示先选。
 */

interface JsonSchemaNode {
  type?: string | string[];
  format?: string;
  description?: string;
  /** schema 缺省值（schemars 对 `#[schemars(default = …)]` 的产物）：store 无值时预填。 */
  default?: unknown;
  enum?: unknown[];
  properties?: Record<string, JsonSchemaNode>;
  required?: string[];
  items?: JsonSchemaNode;
  $ref?: string;
  /** schemars 给「带说明的 $ref 字段」包的一层：`[{ "$ref": … }]`。 */
  allOf?: JsonSchemaNode[];
  /** schemars 对 Rust 枚举生成的分支；字符串枚举会被扁平化成 `enum`。 */
  oneOf?: JsonSchemaNode[];
  /** schemars 对 `Option<T>` 生成的分支：`[T, { type: "null" }]`。 */
  anyOf?: JsonSchemaNode[];
  /** SchemaForm 合成的内部标记（非后端数据）：`Option<T>` 解包后的字段可空，required 不下推。 */
  "x-nullable"?: boolean;
  $defs?: Record<string, JsonSchemaNode>;
  definitions?: Record<string, JsonSchemaNode>;
}

interface Extension {
  "x-visible-when"?: Condition;
  "x-required-when"?: Condition;
  "x-hidden"?: boolean;
  /**
   * 多行文本（后端 schema 注入，如 SSH 档案的私钥 PEM）：string 字段渲染成
   * 文本域而不是单行输入，保留换行，不重排内容。
   */
  "x-multiline"?: boolean;
  /**
   * 下拉选项，由调用方注入（后端 schema 里没有）。用于「字段值是某个资源的 ID」
   * 的场景：把裸数字输入换成按名称选择，选项的 value 保持原始类型。
   */
  "x-options"?: Array<{ value: string | number; label: string }>;
  /**
   * cert.deploy 的 `config`：结构随同级的 `target` 字段变化，后端把各部署
   * 目标的展示名、输入示例与 schema 按目标标识挂在这里。选中目标后按
   * schema 渲染成结构化子表单（见 {@link TargetConfigField}）。
   */
  "x-target-schemas"?: Record<
    string,
    { display_name: string; example: unknown; schema: unknown }
  >;
  /**
   * 字段渲染顺序（调用方注入，后端 schema 里没有）：properties 按该数组
   * 排列，未列出的字段按原序追加在后。用于让逻辑上成对的字段（如
   * cert.deploy SSH 的 cert_path/cert_mode）相邻，不依赖后端 schema 键序。
   */
  "x-field-order"?: string[];
  /**
   * 展示标签（调用方注入，后端 schema 里没有）：Form.Item 的 label 显示它
   * 而不是字段名。用于消除「同名字段在不同路径下语义不同」的歧义（如
   * SSH 变体里顶层的 credential_id 是主机档案、auth 内的是认证凭据）。
   */
  "x-label"?: string;
  /**
   * 强制占整行（调用方注入，后端 schema 里没有）：即使字段本身是单行标量
   * （默认半宽、两个一行）也独占一行。用于布局上「主引用在前、成对明细
   * 在后」的编排（如 SSH 变体的主机档案、重载命令）。
   */
  "x-full-width"?: boolean;
  /**
   * 本字段结束当前行（调用方注入，后端 schema 里没有）：渲染后补一个空列
   * 占满行宽再换行，下一个字段从新行开始。与半宽字段配合实现「独占一行
   * 但右侧留空」的布局（如 SSH 变体的主机档案、重载命令）。
   */
  "x-end-row"?: boolean;
}

type SchemaObject = JsonSchemaNode & Extension;

interface Condition {
  Equals?: { field: string; values: unknown[] };
  /**
   * 数组字段的长度下限（后端 schema 注入，如 cert.apply 的 dns_zone）：
   * `domains` 填了至少 `count` 个域名时命中——其余形态（非数组、字段缺失）
   * 一律按「不满足」处理，不会误标必填。
   */
  MinItems?: { field: string; count: number };
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

/**
 * schemars 对 `Option<T>` 生成的 `anyOf: [T, { type: "null" }]`：解出 `T` 并打上
 * 可空标记——`T` 内层的 required 不能跟着变成表单必填（比如 auth 直填时
 * credential_id 必填，但整字段在「引用主机档案」模式下可以留空）。
 */
function unwrapNullable(node: JsonSchemaNode, root: JsonSchemaNode): JsonSchemaNode {
  const anyOf = node.anyOf;
  if (!Array.isArray(anyOf) || anyOf.length !== 2) return node;
  const nullBranch = anyOf.find((branch) => branch.type === "null");
  const valueBranch = anyOf.find((branch) => branch !== nullBranch);
  if (!nullBranch || !valueBranch) return node;

  const merged: JsonSchemaNode = { ...deref(valueBranch, root), ...node, "x-nullable": true };
  delete merged.anyOf;
  return merged;
}

/**
 * schemars 对 internally tagged 枚举（如 `auth` 的 `kind` 标签）生成的
 * `oneOf` 每支都是一个对象分支：合并成一个对象 schema 才能渲染成普通表单——
 * properties 同名冲突时字符串枚举取并集（`kind` 的 `private_key` / `password`），
 * required 取交集。分支不全是对象时原样返回（字符串枚举交给 flattenEnum）。
 */
function mergeUnionObjects(node: JsonSchemaNode, root: JsonSchemaNode): JsonSchemaNode {
  const oneOf = node.oneOf;
  if (!Array.isArray(oneOf) || oneOf.length === 0) return node;

  const branches = oneOf.map((branch) => deref(branch, root));
  if (!branches.every((branch) => branch.type === "object" && branch.properties)) return node;

  const properties: Record<string, JsonSchemaNode> = {};
  let required: string[] | undefined;
  for (const branch of branches) {
    for (const [name, child] of Object.entries(branch.properties!)) {
      const existing = properties[name];
      properties[name] =
        existing && Array.isArray(existing.enum) && Array.isArray(child.enum)
          ? { ...existing, enum: [...new Set([...existing.enum, ...child.enum])] }
          : (existing ?? child);
    }
    const branchRequired = branch.required ?? [];
    required = required
      ? required.filter((name) => branchRequired.includes(name))
      : [...branchRequired];
  }

  const merged: JsonSchemaNode = { ...node, type: "object", properties };
  delete merged.oneOf;
  if (required && required.length > 0) merged.required = required;
  return merged;
}

/** deref 的出口：`Option<T>` 解包 → tagged-union 合并 → 字符串枚举扁平化。 */
function resolveVariants(node: JsonSchemaNode, root: JsonSchemaNode): JsonSchemaNode {
  const unwrapped = unwrapNullable(node, root);
  const union = mergeUnionObjects(unwrapped, root);
  return union === unwrapped ? flattenEnum(unwrapped) : union;
}

/** 解引用 `$ref`（`#/definitions/…` 或 `#/$defs/…`），并展开枚举。 */
function deref(node: JsonSchemaNode, root: JsonSchemaNode): JsonSchemaNode {
  // 带说明的 $ref 字段会被包成 allOf：先解开内层，再用外层属性（description 等）覆盖。
  if (Array.isArray(node.allOf) && node.allOf.length === 1) {
    const merged: JsonSchemaNode = { ...deref(node.allOf[0], root), ...node };
    delete merged.allOf;
    delete merged.$ref;
    return resolveVariants(merged, root);
  }

  const ref = node.$ref;
  if (ref) {
    const name = ref.replace("#/$defs/", "").replace("#/definitions/", "");
    const defs = root.$defs ?? root.definitions;
    const target = defs?.[name];
    if (target) {
      const merged: JsonSchemaNode = { ...target, ...node };
      delete merged.$ref;
      return resolveVariants(merged, root);
    }
  }

  return resolveVariants(node, root);
}

/** 求值一条条件；条件缺失或形态不认识时按「不满足」处理。 */
function conditionSatisfied(condition: Condition | undefined, values: Record<string, unknown>): boolean {
  const equals = condition?.Equals;
  if (equals) {
    const actual = values[equals.field];
    return actual !== undefined && equals.values.some((value) => value === actual);
  }
  const minItems = condition?.MinItems;
  if (minItems) {
    const actual = values[minItems.field];
    return Array.isArray(actual) && actual.length >= minItems.count;
  }
  return false;
}

/**
 * schema `default` 预填：store 中该路径还没有值时写入缺省值（如 SSH 档案的
 * `user=root`、`cert_mode=0644`）。编辑回填的已有值不覆盖——effect 只在
 * `undefined` 时动手，回填先于或后于挂载发生都安全。
 */
function SchemaDefault({
  node,
  form,
  path,
}: {
  value?: unknown;
  node: JsonSchemaNode;
  form: FormInstance;
  path: (string | number)[];
}) {
  useEffect(() => {
    // 不能用渲染参数 value 判断:它来自 Form.useWatch,首帧是 undefined——
    // 回填的 store 值会被误判成「无值」,default 覆盖之(编辑回填丢失)。
    // 直接读 store 的当前值才可靠。
    if (form.getFieldValue(path) === undefined && node.default !== undefined) {
      form.setFieldValue(path, node.default);
    }
    // node.default 通常是字面量；path 是渲染稳定的表单路径。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [form, node.default, path]);
  return null;
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

  const entries = orderedEntries(properties, (object as SchemaObject)["x-field-order"]);
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
        // 当前取值命中条件（如 directory_url 仅在 ca=custom 时必填、dns_zone
        // 在 domains ≥ 2 时必填）。
        const conditionallyRequired = conditionSatisfied(
          node["x-required-when"],
          values as Record<string, unknown>,
        );
        if (required.has(name) || conditionallyRequired) {
          // 报错文案不复用 description：那是帮助文本，红字里出现会让人以为
          // 「值不合法」，实际只是没填。description 仍作为 tooltip 展示。
          rules.push({ required: true, message: `请填写 ${name}` });
        }

        // 宽度按内容定:变体子表单、多行文本域(如私钥 PEM)占整行,
        // 其余单行标量两个一行。与 ObjectFieldGroup 的子字段规则一致。
        // type 可能是数组(Option<T> 生成 ["string","null"]),取第一个真实类型。
        const topType = Array.isArray(node.type) ? node.type[0] : node.type;
        const topIsScalar =
          topType === "integer" || topType === "number" || topType === "boolean" || topType === "string";
        const span = node["x-target-schemas"] || node["x-multiline"] || !topIsScalar ? 24 : 12;

        return (
          <Col span={span} key={path.join(".")}>
            <SchemaDefault
              value={(values as Record<string, unknown>)?.[name]}
              node={node}
              form={form}
              path={path}
            />
            <Form.Item
              name={path}
              label={node["x-label"] ?? name}
              tooltip={node.description}
              rules={rules}
              valuePropName={node.type === "boolean" ? "checked" : "value"}
            >
              <SchemaField
                node={node}
                form={form}
                path={path}
                root={root}
                values={values as Record<string, unknown>}
                disabled={disabled}
              />
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
  values,
  disabled,
  ...controlProps
}: {
  node: SchemaObject;
  form: FormInstance;
  path: (string | number)[];
  /** 根 schema，用于解析嵌套字段里的 `$ref`。 */
  root: JsonSchemaNode;
  /** 当前字段所在对象的整体取值，供依赖同级字段（如 target）的扩展读取。 */
  values?: Record<string, unknown>;
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
      // 一层嵌套对象：分组渲染（如 DNS 提供商的嵌套字段、cert.deploy 的 config）。
      const nested = node.properties;
      if (nested && Object.keys(nested).length > 0) {
        return (
          <ObjectFieldGroup
            properties={nested}
            root={root}
            parentPath={path}
            parentValues={values}
            form={form}
            requiredNames={undefined}
            disabled={disabled}
          />
        );
      }
      return <JsonTextArea disabled={disabled} {...control} />;
    }
    case "string":
      // 多行文本（如 SSH 档案的私钥 PEM）：整段占满、保留换行，
      // 失焦也不重排——粘贴进来的 PEM 逐字节原样提交。
      if (node["x-multiline"]) {
        return (
          <Input.TextArea
            disabled={disabled}
            autoSize={{ minRows: 4, maxRows: 16 }}
            {...control}
          />
        );
      }
      return <Input disabled={disabled} {...control} />;
    default:
      // 没有可识别类型（如 `serde_json::Value`，结构随所选目标变化）：
      // 带 x-target-schemas 时渲染结构化变体表单，否则按 JSON 文本处理，
      // 别退化成单行文本框让人手打整个对象。
      return <TargetConfigField node={node} values={values} form={form} path={path} disabled={disabled} {...control} />;
  }
}

/**
 * 嵌套对象的字段组：每个子字段带 label（字段名）与说明（tooltip），
 * `requiredNames` 给出的字段追加必填规则——用于把部署目标 schema 的
 * required 下推到 `config` 子表单（`x-nullable` 字段豁免：可空的内层
 * required 不是表单必填，见 {@link unwrapNullable}）。
 */
/**
 * 按 `x-field-order` 声明重排字段：声明的名字按声明序排在前面，未声明的
 * 字段保持原序追加在后，因此漏配不会丢字段。未声明顺序时原样返回。
 */
function orderedEntries(
  properties: Record<string, JsonSchemaNode>,
  order: string[] | undefined,
): [string, JsonSchemaNode][] {
  const entries = Object.entries(properties);
  if (!order || order.length === 0) return entries;
  const known = new Map(entries);
  const head = order.filter((name) => known.has(name));
  const rest = entries.filter(([name]) => !order.includes(name));
  return [...head.map((name) => [name, known.get(name)!] as [string, JsonSchemaNode]), ...rest];
}

function ObjectFieldGroup({
  properties,
  root,
  parentPath,
  parentValues,
  form,
  requiredNames,
  disabled,
}: {
  properties: Record<string, JsonSchemaNode>;
  /** 子字段的 `$ref` 相对哪个 schema 解析（变体子表单传目标自己的 schema）。 */
  root: JsonSchemaNode;
  parentPath: (string | number)[];
  /** 父对象的当前取值，供子字段读取同级值（联动扩展）。 */
  parentValues?: Record<string, unknown>;
  form: FormInstance;
  /** 需要必填校验的字段名；`undefined` 表示不追加（如普通嵌套对象保持宽松）。 */
  requiredNames: ReadonlySet<string> | undefined;
  disabled?: boolean;
}) {
  // 字段顺序是根 schema（或变体 schema，即这里的 root）上的声明。
  const entries = orderedEntries(properties, (root as SchemaObject)["x-field-order"]);
  return (
    <Row gutter={[16, 0]}>
      {entries.map(([name, rawNode]) => {
        const child = deref(rawNode, root) as SchemaObject;
        const childPath = [...parentPath, name];
        const childType = Array.isArray(child.type) ? child.type[0] : child.type;
        const rules: Rule[] = [];
        if (requiredNames?.has(name) && !child["x-nullable"]) {
          rules.push({ required: true, message: `请填写 ${name}` });
        }
        // 宽度按内容定：单行标量（含普通字符串路径、端口、权限位、uid/gid、
        // 凭据/枚举下拉）两个一行；文本域、嵌套结构占整行。
        const isScalar =
          childType === "integer" ||
          childType === "number" ||
          childType === "boolean" ||
          childType === "string";
        const span = child["x-full-width"] || child["x-multiline"] || !isScalar ? 24 : 12;
        return (
          <Fragment key={childPath.join(".")}>
            <Col span={span}>
              <SchemaDefault value={parentValues?.[name]} node={child} form={form} path={childPath} />
              <Form.Item
                name={childPath}
                label={child["x-label"] ?? name}
                tooltip={child.description}
                rules={rules}
                valuePropName={childType === "boolean" ? "checked" : "value"}
              >
                <SchemaField
                  node={child}
                  form={form}
                  path={childPath}
                  root={root}
                  values={(parentValues?.[name] ?? {}) as Record<string, unknown>}
                  disabled={disabled}
                />
              </Form.Item>
            </Col>
            {/* 行尾标记：补空列占满剩余宽度，下一个字段从新行开始——
                独占一行但右侧留空，而不是让后面的字段补位上来。 */}
            {child["x-end-row"] && span === 12 && <Col span={12} aria-hidden />}
          </Fragment>
        );
      })}
    </Row>
  );
}

/**
 * 无类型字段的编辑器（cert.deploy 的 `config`：结构随同级 `target` 变化）：
 * 带后端注入的 `x-target-schemas` 时按所选目标渲染**结构化子表单**——字段名、
 * 说明、必填都来自目标声明的 schema；未选 target 时提示先选；目标没声明
 * 字段结构时退回 JSON 文本编辑，不把用户锁死在空表单里。
 *
 * 切换 target 时清空该字段的存量值：A 目标的字段残留进 B 目标的输入虽会被
 * 后端忽略，但界面上两套字段混在一起，会让人误以为残留值仍在生效。
 */
function TargetConfigField({
  node,
  values,
  form,
  path,
  disabled,
  ...controlProps
}: {
  node: SchemaObject;
  values?: Record<string, unknown>;
  form: FormInstance;
  path: (string | number)[];
  disabled?: boolean;
} & Record<string, unknown>) {
  const variants = node["x-target-schemas"];
  // 不带扩展的普通无类型字段（如别的 serde_json::Value 输入）：与旧版一致，
  // 走 JSON 文本编辑，不套 target 联动。
  if (!variants) {
    return <JsonTextArea disabled={disabled} {...controlProps} />;
  }
  const target = typeof values?.target === "string" ? values.target : "";
  const variant = target === "" ? undefined : variants?.[target];
  const variantSchema = variant?.schema as JsonSchemaNode | undefined;
  const properties = variantSchema?.properties;

  // 挂载时的 target 不触发清空（编辑既有流水线时回填的值不能丢）。
  // 不能用 ref 初值判断：Form.useWatch 首帧返回 undefined（ref 被初始化成 ""），
  // 下一帧同步到回填的真实值时会被误判成「用户切换了 target」而清空 config。
  // 改为「见过非空 target 后才允许清空」：首帧 → 回填值的过渡不算切换。
  const [hasTarget, setHasTarget] = useState(target !== "");
  useEffect(() => {
    if (target === "") return;
    setHasTarget(true);
  }, [target]);
  const lastTarget = useRef<string | null>(target !== "" ? target : null);
  useEffect(() => {
    if (target === "") {
      // 用户显式清掉 target 选择时保持行尾语义：本字段也一并清空。
      if (hasTarget) form.setFieldValue(path, undefined);
      return;
    }
    const previous = lastTarget.current;
    lastTarget.current = target;
    if (previous !== null && previous !== target && hasTarget) {
      form.setFieldValue(path, undefined);
    }
  }, [form, hasTarget, path, target]);

  if (target === "") {
    const options = Object.entries(variants ?? {})
      .map(([id, item]) => `${id}：${item.display_name}`)
      .join("；");
    return <Input disabled placeholder={`先选择 target（${options}）`} />;
  }
  if (!properties || Object.keys(properties).length === 0) {
    return <JsonTextArea disabled={disabled} {...controlProps} />;
  }
  return (
    <ObjectFieldGroup
      properties={properties}
      root={variantSchema as JsonSchemaNode}
      parentPath={path}
      parentValues={(controlProps.value ?? {}) as Record<string, unknown>}
      form={form}
      requiredNames={new Set(variantSchema?.required ?? [])}
      disabled={disabled}
    />
  );
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
