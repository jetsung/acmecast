import { useQuery, useQueryClient } from "@tanstack/react-query";
import { App, Button, Card, Form, Input, Select, Space, Switch, Typography } from "antd";
import { useEffect, useMemo, useRef, useState } from "react";
import { useLocation, useNavigate, useParams } from "react-router";
import type { components } from "@/api/schema";
import { client } from "@/api/client";
import { unwrapData } from "@/api/helpers";
import { SchemaForm } from "@/components/SchemaForm";
import { PageHeader } from "@/components/PageHeader";

/** `account_credential_id` 一类字段只接受这类凭据。 */
const ACME_ACCOUNT_TYPE_ID = "acme.account";
/** SSH 主机档案凭据：cert.deploy 的 SSH 配置按标识引用它。 */
const SSH_HOST_TYPE_ID = "ssh";

const DNS_CREDENTIAL_TYPE_IDS: Record<string, true> = {
  cloudflare: true,
  aliyun: true,
  tencent: true,
  "tencent-eo": true,
};

/** 与后端 acmecast_store::capability::CONTROLLED_TYPES 保持一致。 */
const CAPABILITY_CONTROLLED_TYPES = ["tencent", "cloudflare", "aliyun"];

/**
 * 能力过滤：受控类型（tencent/cloudflare/aliyun）需声明指定能力；
 * 能力缺省（旧数据 / 非受控类型如 tencent-eo）视为双能力，不因能力被过滤。
 */
function hasCapability(
  credential: components["schemas"]["CredentialResponse"],
  capability: "dns" | "deploy",
): boolean {
  if (!CAPABILITY_CONTROLLED_TYPES.includes(credential.type_id)) return true;
  const caps = credential.capabilities ?? [];
  return caps.length === 0 || caps.includes(capability);
}

/** 内置部署目标（与 `steps::default_deploy_registry()` 对齐）。 */
const DEPLOY_TARGETS = [
  { value: "local", label: "本地文件系统" },
  { value: "ssh", label: "SSH 远程主机" },
  { value: "tencent-ssl", label: "腾讯云 SSL 证书" },
  { value: "s3", label: "S3 兼容存储" },
  { value: "webdav", label: "WebDAV" },
];

interface FieldOption {
  value: string | number;
  label: string;
}

/**
 * 字段名 → 选项来源：取值是「某个资源的 ID」或「固定的几个标识」时改成下拉，
 * 免得手填一个界面上看不见的主键、或手打一个标识。
 *
 * 按字段名而不是任务类型匹配：同名语义一致（如任何任务里的
 * `account_credential_id` 都是 ACME 账号凭据）。
 */
const FIELD_OPTIONS: Record<
  string,
  { fromCredentials: (credential: components["schemas"]["CredentialResponse"]) => boolean }
  | { fixed: FieldOption[] }
> = {
  account_credential_id: { fromCredentials: (c) => c.type_id === ACME_ACCOUNT_TYPE_ID },
  acme_account_credential_id: { fromCredentials: (c) => c.type_id === ACME_ACCOUNT_TYPE_ID },
  // DNS-01 挑战凭据：类型白名单 + 需声明 dns 能力（缺省视为具备）。
  dns_credential_id: {
    fromCredentials: (c) => !!DNS_CREDENTIAL_TYPE_IDS[c.type_id] && hasCapability(c, "dns"),
  },
  // cert.deploy SSH 配置里的凭据引用：主机档案（顶层）与认证凭据（auth 内）。
  credential_id: { fromCredentials: (c) => c.type_id === SSH_HOST_TYPE_ID },
  // 部署目标类型；当前只有 cert.deploy 用它。
  target: { fixed: DEPLOY_TARGETS },
};

/** 字段名 → 该字段的可选项。 */
type FieldOptions = Record<string, FieldOption[]>;

interface StepRow {
  /** 本地稳定 id：任务输入按它挂到表单路径上，增删/排序都不会串值。 */
  id: number;
  type_id: string;
  enabled: boolean;
}

interface PipelineForm {
  name: string;
  description?: string | null;
  enabled: boolean;
}

interface TaskType {
  type_id: string;
  display_name: string;
}

interface CopyState {
  name: string;
  description: string | null;
  enabled: boolean;
  steps: Array<{ type_id: string; input: Record<string, unknown>; enabled: boolean }>;
}

const SINGLETON_STEP_TYPES: Record<string, true> = {
  "cert.apply": true,
  "cert.store": true,
};

/** 流水线编辑器（3.2）：步骤增删/排序，输入表单由任务 schema 动态渲染。 */
export function PipelineEditorPage() {
  const { id } = useParams();
  const editingId = id === "new" ? null : Number(id);
  const navigate = useNavigate();
  const location = useLocation();
  const { message } = App.useApp();
  const queryClient = useQueryClient();
  const [form] = Form.useForm();
  const [steps, setSteps] = useState<StepRow[]>([]);
  const [saving, setSaving] = useState(false);
  // 步骤本地 id 生成器；只增不减，删除后不复用，避免残留的表单值被误读。
  const nextStepId = useRef(1);

  const { data: tasks } = useQuery({
    queryKey: ["tasks"],
    queryFn: async () => {
      const result = await client.GET("/api/tasks", {});
      return unwrapData<TaskType[]>(result);
    },
  });

  // 凭据列表：把「凭据 ID」字段渲染成按名称选择的下拉。
  const { data: credentials } = useQuery({
    queryKey: ["credentials"],
    queryFn: async () => {
      const result = await client.GET("/api/credentials", {
        params: { query: { page: 1, page_size: 100 } },
      });
      return unwrapData<{ items: components["schemas"]["CredentialResponse"][] }>(result);
    },
  });
  const credentialItems = credentials?.items ?? [];
  const fieldOptions = useMemo<FieldOptions>(() => {
    const items = credentialItems;
    const resolved: FieldOptions = {};
    for (const [field, source] of Object.entries(FIELD_OPTIONS)) {
      resolved[field] =
        "fixed" in source
          ? source.fixed
          : items
              .filter((credential) => source.fromCredentials(credential))
              .map((credential) => ({
                value: credential.id,
                label: credential.remark
                  ? `${credential.name}（id=${credential.id}）— ${credential.remark}`
                  : `${credential.name}（id=${credential.id}）`,
              }));
    }
    return resolved;
  }, [credentialItems]);

  // 复制预填：从列表页 navigate state 带入的新建模板
  const copyFrom = (location.state as { copyFrom?: CopyState } | null)?.copyFrom;

  // 复制进入新建：预填表单与步骤
  useEffect(() => {
    if (editingId !== null || copyFrom === undefined) return;
    // 避免重复预填（React StrictMode 会双跑 effect）
    if (steps.length > 0) return;
    form.setFieldsValue({
      name: copyFrom.name,
      description: copyFrom.description ?? undefined,
      enabled: copyFrom.enabled,
    });
    const loaded = copyFrom.steps.map((step) => ({
      id: nextStepId.current++,
      type_id: step.type_id,
      enabled: step.enabled,
    }));
    setSteps(loaded);
    form.setFieldsValue({
      inputs: Object.fromEntries(
        loaded.map((step, index) => [step.id, (copyFrom.steps[index].input ?? {}) as Record<string, unknown>]),
      ),
    });
  }, [editingId, copyFrom, form, steps.length]);

  // 编辑模式：载入既有定义。
  useEffect(() => {
    if (editingId === null) return;
    void (async () => {
      const result = await client.GET(`/api/pipelines/${editingId}`, {
        params: { path: { id: editingId } },
      });
      void 0;
      if (result.error !== undefined) {
        message.error("流水线不存在");
        navigate("/pipelines");
        return;
      }
      const full = await unwrapData<components["schemas"]["PipelineResponse"]>(result);
      form.setFieldsValue({
        name: full.name,
        description: full.description ?? undefined,
        enabled: full.enabled,
      });
      const loaded = full.steps.map((step) => ({
        id: nextStepId.current++,
        type_id: step.type_id,
        enabled: step.enabled,
      }));
      setSteps(loaded);
      // 把已存的步骤输入回填到表单 store：SchemaForm 按 ["inputs", id] 读取，
      // 与步骤顺序解耦（所以排序、删除都不会让输入错位）。
      form.setFieldsValue({
        inputs: Object.fromEntries(
          loaded.map((step, index) => [step.id, (full.steps[index].input ?? {}) as Record<string, unknown>]),
        ),
      });
    })();
  }, [editingId, form, message, navigate]);

  const addStep = () => {
    const pickable = (tasks ?? []).find((t) => canAddType(t.type_id));
    const fallback = tasks?.[0]?.type_id ?? "";
    const typeId = pickable?.type_id ?? fallback;
    if (!canAddType(typeId)) {
      message.warning(`${typeId} 仅允许添加一次`);
      return;
    }
    setSteps((current) => [...current, { id: nextStepId.current++, type_id: typeId, enabled: true }]);
  };

  const removeStep = (id: number) => {
    setSteps((current) => current.filter((step) => step.id !== id));
  };

  const move = (index: number, delta: number) => {
    setSteps((current) => {
      const next = [...current];
      const target = index + delta;
      if (target < 0 || target >= next.length) return current;
      [next[index], next[target]] = [next[target], next[index]];
      return next;
    });
  };

  // renew_before_days 仍在 cert.apply 的 schema 中按表单展示（由后端 JsonSchema 渲染），
  // 不再联动调度自动创建；到期跳过在步骤执行态中完成（0/负数=跳过检测）。

  const stepTypeCount: Record<string, number> = {};
  for (const s of steps) {
    const key = s.type_id.trim();
    if (key) stepTypeCount[key] = (stepTypeCount[key] ?? 0) + 1;
  }
  const hasApply = (stepTypeCount["cert.apply"] ?? 0) >= 1;
  const hasStore = (stepTypeCount["cert.store"] ?? 0) >= 1;

  // cert.store 默认值联动 cert.apply：显式字段，默认跟随 apply 的 account_credential_id
  const inputsWatch = Form.useWatch(["inputs"], form) as Record<string, Record<string, unknown>> | undefined;
  useEffect(() => {
    const applyStep = steps.find((s) => s.type_id === "cert.apply");
    const storeStep = steps.find((s) => s.type_id === "cert.store");
    if (!applyStep || !storeStep) return;
    const applyVal = (inputsWatch?.[String(applyStep.id)] as Record<string, unknown> | undefined)?.["account_credential_id"];
    if (applyVal == null || applyVal === "") return;
    const storeVal = (inputsWatch?.[String(storeStep.id)] as Record<string, unknown> | undefined)?.["acme_account_credential_id"];
    // 仅当 store 字段为空时自动填入 apply 的值，保留用户显式覆盖
    if (storeVal == null || storeVal === "") {
      form.setFieldValue(["inputs", storeStep.id, "acme_account_credential_id"], applyVal);
    }
  }, [steps, inputsWatch, form]);

  const canAddType = (typeId: string) => {
    const trimmed = typeId.trim();
    if (SINGLETON_STEP_TYPES[trimmed]) {
      return (stepTypeCount[trimmed] ?? 0) === 0;
    }
    return true;
  };
  const onSave = async (values: PipelineForm) => {
    // 前端即时拦截：cert.apply / cert.store 各至多一次
    const applyCount = stepTypeCount["cert.apply"] ?? 0;
    const storeCount = stepTypeCount["cert.store"] ?? 0;
    if (applyCount > 1) {
      message.error("cert.apply 仅允许添加一次");
      return;
    }
    if (storeCount > 1) {
      message.error("cert.store 仅允许添加一次");
      return;
    }
    // renew_before_days：0 表示跳过过期校验，>0 为到期阈值（正整数校验）；缺省按 30 补齐
    for (const step of steps) {
      if (step.type_id !== "cert.apply") continue;
      const raw = (form.getFieldValue(["inputs", step.id]) ?? {}) as Record<string, unknown>;
      const input = raw as Record<string, unknown>;
      const value = input["renew_before_days"];
      const missing =
        value === undefined ||
        value === null ||
        (typeof value === "string" && (value as string).trim() === "");
      if (missing) {
        form.setFieldValue(["inputs", step.id, "renew_before_days"], 30);
        continue;
      }
      let num: number | null = null;
      if (typeof value === "number") {
        num = value as number;
      } else if (typeof value === "string") {
        const trimmed = (value as string).trim();
        if (trimmed === "") {
          form.setFieldValue(["inputs", step.id, "renew_before_days"], 30);
          continue;
        }
        const parsed = Number(trimmed);
        if (!Number.isFinite(parsed)) {
          message.error("renew_before_days：必须为大于等于 0 的整数");
          return;
        }
        num = parsed;
      } else {
        message.error("renew_before_days：必须为大于等于 0 的整数");
        return;
      }
      if (!Number.isInteger(num) || num < 0) {
        message.error("renew_before_days：必须为大于等于 0 的整数");
        return;
      }
      // 字符串输入归一为数字
      if (typeof value === "string") {
        form.setFieldValue(["inputs", step.id, "renew_before_days"], num);
      }
    }
    setSaving(true);
    try {
      const body: Record<string, unknown> = {
        name: values.name,
        description: values.description ?? null,
        enabled: values.enabled,
        steps: steps.map((step) => ({
          type_id: step.type_id,
          input: (form.getFieldValue(["inputs", step.id]) ?? {}) as Record<string, unknown>,
          enabled: step.enabled,
        })),
      };
      const result =
        editingId === null
          ? await client.POST("/api/pipelines", { body: body as never })
          : await client.PUT(`/api/pipelines/${editingId}`, {
              params: { path: { id: editingId } },
              body: body as never,
            });
      if (result.error !== undefined) {
        const errorBody = result.error as { error?: { message?: string; field?: string } };
        if (errorBody?.error?.field) {
          message.error(`${errorBody.error.field}：${errorBody.error.message}`);
          return;
        }
        message.error(errorBody?.error?.message ?? "保存失败");
        return;
      }
      message.success("流水线已保存");
      await queryClient.invalidateQueries({ queryKey: ["pipelines"] });
      navigate("/pipelines");
    } finally {
      setSaving(false);
    }
  };

  return (
    <div>
      <PageHeader
        title={editingId === null ? "新建流水线" : `编辑流水线 #${editingId}`}
        description="按执行顺序组织 DNS 挑战、签发与部署步骤"
      />
      <Form form={form} layout="vertical" onFinish={onSave}>
        <Space size={24} wrap>
          <Form.Item
            name="name"
            label="名称"
            rules={[{ required: true, message: "请输入名称" }]}
          >
            <Input style={{ width: 240 }} />
          </Form.Item>
          <Form.Item name="description" label="描述">
            <Input style={{ width: 320 }} />
          </Form.Item>
          <Form.Item name="enabled" label="启用" valuePropName="checked" initialValue={true}>
            <Switch />
          </Form.Item>
        </Space>

        <Typography.Title level={5}>步骤</Typography.Title>
        {steps.map((step, index) => (
          <Card
            key={step.id}
            size="small"
            title={`步骤 ${index + 1}`}
            extra={
              <Space>
                <Button size="small" onClick={() => move(index, -1)} disabled={index === 0}>
                  上移
                </Button>
                <Button
                  size="small"
                  onClick={() => move(index, 1)}
                  disabled={index === steps.length - 1}
                >
                  下移
                </Button>
                <Button size="small" danger onClick={() => removeStep(step.id)}>
                  移除
                </Button>
              </Space>
            }
            style={{ marginBottom: 16 }}
          >
            <Space size={16} wrap style={{ marginBottom: 8 }}>
              <span>任务类型：</span>
              <Select
                style={{ width: 240 }}
                value={step.type_id}
                options={(tasks ?? []).map((task) => {
                  const disabled =
                    SINGLETON_STEP_TYPES[task.type_id] &&
                    task.type_id !== step.type_id &&
                    ((task.type_id === "cert.apply" && hasApply) || (task.type_id === "cert.store" && hasStore));
                  return {
                    value: task.type_id,
                    label: task.display_name,
                    disabled,
                  };
                })}
                onChange={(value) => {
                  if (!canAddType(value) && value !== step.type_id) {
                    message.warning(`${value} 仅允许添加一次`);
                    return;
                  }
                  setSteps((current) =>
                    current.map((item) =>
                      item.id === step.id ? { ...item, type_id: value } : item,
                    ),
                  );
                  // 换类型清空该步骤的输入，避免上一类型的残留字段混入新类型。
                  form.setFieldValue(["inputs", step.id], {});
                }}
              />
              <Switch
                checked={step.enabled}
                onChange={(value) => {
                  setSteps((current) =>
                    current.map((item) =>
                      item.id === step.id ? { ...item, enabled: value } : item,
                    ),
                  );
                }}
              />
            </Space>
            <TaskSchemaFields
              typeId={step.type_id}
              stepId={step.id}
              fieldOptions={fieldOptions}
              rawCredentials={credentialItems}
            />
          </Card>
        ))}
        <Button onClick={addStep} style={{ marginBottom: 24 }}>
          添加步骤
        </Button>

        <div>
          <Button type="primary" htmlType="submit" loading={saving}>
            保存
          </Button>
          <Button style={{ marginLeft: 12 }} onClick={() => navigate("/pipelines")}>
            取消
          </Button>
        </div>
      </Form>
    </div>
  );
}

/** 单个步骤的任务输入表单：从后端拉 schema 并交给 SchemaForm 渲染。 */
function TaskSchemaFields({
  typeId,
  stepId,
  fieldOptions,
  rawCredentials,
}: {
  typeId: string;
  stepId: number;
  fieldOptions: FieldOptions;
  rawCredentials: components["schemas"]["CredentialResponse"][];
}) {
  const { data: schema } = useQuery({
    queryKey: ["task-schema", typeId],
    queryFn: async () => {
      const result = await client.GET("/api/tasks/{type_id}/schema", {
        params: { path: { type_id: typeId } },
      });
      return unwrapData<unknown>(result);
    },
    enabled: typeId !== "",
  });

  const decorated = useMemo(
    () => withFieldOptions(schema, fieldOptions, rawCredentials),
    [schema, fieldOptions, rawCredentials],
  );

  if (!schema) {
    return <Typography.Text type="secondary">（无输入定义）</Typography.Text>;
  }
  // 输入值挂在 ["inputs", stepId] 路径下：用稳定 id 而非下标，步骤排序/删除后不会串值。
  return <SchemaFormBridge schema={decorated} prefix={["inputs", stepId]} />;
}

/**
 * SSH 变体表单只保留的字段：`credential_id` 引用 SSH 主机档案——连接信息
 * （host/port/user）与认证材料在执行时从档案取，权限位用档案值或系统缺省；
 * 其余三个是部署输入，绑定「证书装到哪、怎么重载」，档案不携带。
 */
const SSH_FIELDS = ["credential_id", "cert_path", "key_path", "reload_command"] as const;

/**
 * local 变体的字段渲染顺序：路径与其权限位成对相邻（两字段半宽即同行），
 * uid/gid 成对在后；重载命令最后。字段全部保留，仅声明顺序。
 */
const LOCAL_FIELD_ORDER = [
  "cert_path",
  "cert_mode",
  "key_path",
  "key_mode",
  "uid",
  "gid",
  "reload_command",
];

/**
 * 保留字段的展示标签（`x-label`）：`credential_id` 不显示裸字段名，
 * 让「从凭据取连接信息」的语义在表单上可读。
 */
const SSH_LABELS: Record<string, string> = {
  credential_id: "SSH 主机档案",
};

/**
 * 收紧 SSH 变体 schema：只保留 `SSH_FIELDS` 声明的字段，并注入展示标签与
 * 行尾标记——「SSH 主机档案」与「重载命令」各独占一行（右侧空列占位），
 * `cert_path`/`key_path` 成对同行。连接、认证、权限字段从表单剔除——后端
 * `SshInput` 仍接受它们（兼容旧数据），但界面上不再提供入口。
 */
function tightenSshSchema(schema: Record<string, unknown>): Record<string, unknown> {
  const props = schema.properties as Record<string, Record<string, unknown>> | undefined;
  if (!props) return schema;
  const next: Record<string, Record<string, unknown>> = {};
  for (const field of SSH_FIELDS) {
    const node = props[field];
    if (!node) continue;
    const decorated: Record<string, unknown> = { ...node };
    if (SSH_LABELS[field]) decorated["x-label"] = SSH_LABELS[field];
    // 主引用与重载命令独占一行，右侧空列占位，不让路径字段补位上来。
    if (field === "credential_id" || field === "reload_command") {
      decorated["x-end-row"] = true;
    }
    next[field] = decorated;
  }
  return { ...schema, properties: next, "x-field-order": [...SSH_FIELDS] };
}

/**
 * cert.deploy 输入的字段渲染顺序：`target`（选哪种部署方式）是首要决策，
 * 排最前；`force`（要不要强制重写）次之；`config` 子表单跟在决策字段之后。
 */
const CERT_DEPLOY_FIELD_ORDER = ["target", "config"];

/**
 * 给「应该选一个」的字段注入 `x-options`（SchemaForm 的客户端扩展），
 * 把裸输入换成下拉；值仍是原来的类型（凭据主键是数字，标识是字符串），后端不用改。
 *
 * `x-target-schemas` 的变体子字段（cert.deploy 的 config）同样按字段名注入：
 * 目标 schema 是后端动态给的，注入必须在这里递归进行。SSH 变体额外注入
 * `x-field-order`，保证 host/port 等成对字段相邻，不依赖后端 schema 键序。
 */
function withFieldOptions(
  schema: unknown,
  options: FieldOptions,
  rawCredentials: components["schemas"]["CredentialResponse"][],
): unknown {
  const root = schema as
    | { properties?: Record<string, Record<string, unknown>> }
    | undefined;
  const properties = root?.properties;
  if (!properties) return schema;

  const next = { ...properties };
  let changed = false;
  for (const [field, fieldOptions] of Object.entries(options)) {
    if (!next[field]) continue;
    next[field] = { ...next[field], "x-options": fieldOptions };
    changed = true;
  }
  if (next["force"]) {
    next["force"] = { ...(next["force"] as Record<string, unknown>), "x-hidden": true };
    changed = true;
  }
  // cert.store：acme_account_credential_id 显式展示，描述提示默认跟随 cert.apply
  if (next["acme_account_credential_id"]) {
    const field = next["acme_account_credential_id"] as Record<string, unknown>;
    const desc = (field.description as string | undefined) ?? "";
    if (!desc.includes("默认跟随")) {
      next["acme_account_credential_id"] = {
        ...field,
        description: desc
          ? `${desc}（默认跟随 cert.apply 的 account_credential_id，可显式覆盖）`
          : "默认跟随 cert.apply 的 account_credential_id，可显式覆盖",
      };
      changed = true;
    }
  }
  // cert.deploy 的输入 schema（以 dns_provider/target/config 为特征）注入根级
  // 字段顺序：force/target 两个决策字段排最前，config 子表单跟在后面。
  const isCertDeploy = Boolean(next.target && next.config && (next.config as SchemaOptionsNode)["x-target-schemas"]);
  const order = isCertDeploy ? CERT_DEPLOY_FIELD_ORDER : undefined;
  for (const [field, node] of Object.entries(next)) {
    const variants = (node as SchemaOptionsNode)["x-target-schemas"];
    if (!variants) continue;
    const injectedVariants: typeof variants = {};
    for (const [targetId, variant] of Object.entries(variants)) {
      const injected = withFieldOptions(variant.schema, options, rawCredentials) as Record<
        string,
        unknown
      >;
      // SSH 变体收紧为档案引用 + 部署输入（连接/认证/权限由档案提供）；
      // local 变体字段全部保留，仅声明成对相邻的渲染顺序。
      let tightened: Record<string, unknown> =
        targetId === "ssh"
          ? tightenSshSchema(injected)
          : targetId === "local"
            ? { ...injected, "x-field-order": LOCAL_FIELD_ORDER }
            : injected;
      // tencent-ssl 变体的 credential_id 后端注入了 x-credential-types 白名单
      // （仅 tencent）：该变体的下拉需按白名单过滤，否则会用 FIELD_OPTIONS 的
      // credential_id（仅 SSH）给 tencent-ssl 注入错误选项。
      const props = tightened.properties as
        | Record<string, Record<string, unknown>>
        | undefined;
      const credentialNode = props?.["credential_id"] as
        | Record<string, unknown>
        | undefined;
      const allowlist = credentialNode?.["x-credential-types"] as
        | string[]
        | undefined;
      if (Array.isArray(allowlist) && allowlist.length > 0) {
        const allow = new Set(allowlist);
        // 类型白名单之外再按能力过滤：tencent-ssl 部署需凭据声明 deploy 能力
        // （受控类型缺省视为双能力，非受控类型本就不会命中白名单）。
        const filtered = rawCredentials
          .filter(
            (credential) => allow.has(credential.type_id) && hasCapability(credential, "deploy"),
          )
          .map((credential) => ({
            value: credential.id,
            label: credential.remark
              ? `${credential.name}（id=${credential.id}）— ${credential.remark}`
              : `${credential.name}（id=${credential.id}）`,
          }));
        tightened = {
          ...tightened,
          properties: {
            ...props,
            credential_id: {
              ...credentialNode,
              "x-options": filtered,
            },
          },
        };
      }
      injectedVariants[targetId] = { ...variant, schema: tightened };
    }
    next[field] = { ...node, "x-target-schemas": injectedVariants };
    changed = true;
  }
  return changed
    ? { ...root, properties: next, ...(order ? { "x-field-order": order } : {}) }
    : schema;
}

/** `withFieldOptions` 递归时用到的最小节点形状（完整 schema 由变体自身保证）。 */
interface SchemaOptionsNode {
  "x-target-schemas"?: Record<
    string,
    { display_name: string; example: unknown; schema: unknown }
  >;
}

function SchemaFormBridge({
  schema,
  prefix,
}: {
  schema: unknown;
  prefix: (string | number)[];
}) {
  const form = Form.useFormInstance();
  return <SchemaForm schema={schema} form={form} namePrefix={prefix} />;
}
