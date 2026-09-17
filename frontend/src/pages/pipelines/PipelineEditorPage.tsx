import { useQuery, useQueryClient } from "@tanstack/react-query";
import { App, Button, Card, Form, Input, Select, Space, Switch, Typography } from "antd";
import { useEffect, useState } from "react";
import { useNavigate, useParams } from "react-router";
import type { components } from "@/api/schema";
import { client } from "@/api/client";
import { unwrapData } from "@/api/helpers";
import { SchemaForm } from "@/components/SchemaForm";

interface StepRow {
  type_id: string;
  input: Record<string, unknown>;
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

/** 流水线编辑器（3.2）：步骤增删/排序，输入表单由任务 schema 动态渲染。 */
export function PipelineEditorPage() {
  const { id } = useParams();
  const editingId = id === "new" ? null : Number(id);
  const navigate = useNavigate();
  const { message } = App.useApp();
  const queryClient = useQueryClient();
  const [form] = Form.useForm();
  const [steps, setSteps] = useState<StepRow[]>([]);
  const [saving, setSaving] = useState(false);

  const { data: tasks } = useQuery({
    queryKey: ["tasks"],
    queryFn: async () => {
      const result = await client.GET("/api/tasks", {});
      return unwrapData<TaskType[]>(result);
    },
  });

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
      setSteps(
        full.steps.map((step) => ({
          type_id: step.type_id,
          input: (step.input ?? {}) as Record<string, unknown>,
          enabled: step.enabled,
        })),
      );
    })();
  }, [editingId, form, message, navigate]);

  const addStep = () => {
    setSteps((current) => [...current, { type_id: tasks?.[0]?.type_id ?? "", input: {}, enabled: true }]);
  };

  const removeStep = (index: number) => {
    setSteps((current) => current.filter((_, i) => i !== index));
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

  const onSave = async (values: PipelineForm) => {
    setSaving(true);
    try {
      const body = {
        name: values.name,
        description: values.description ?? null,
        enabled: values.enabled,
        steps,
      };
      const result =
        editingId === null
          ? await client.POST("/api/pipelines", { body })
          : await client.PUT(`/api/pipelines/${editingId}`, {
              params: { path: { id: editingId } },
              body,
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
      <Typography.Title level={4}>
        {editingId === null ? "新建流水线" : `编辑流水线 #${editingId}`}
      </Typography.Title>
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
            key={index}
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
                <Button size="small" danger onClick={() => removeStep(index)}>
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
                options={(tasks ?? []).map((task) => ({
                  value: task.type_id,
                  label: task.display_name,
                }))}
                onChange={(value) => {
                  setSteps((current) =>
                    current.map((item, i) =>
                      i === index ? { ...item, type_id: value, input: {} } : item,
                    ),
                  );
                }}
              />
              <span>启用：</span>
              <Switch
                checked={step.enabled}
                onChange={(value) => {
                  setSteps((current) =>
                    current.map((item, i) => (i === index ? { ...item, enabled: value } : item)),
                  );
                }}
              />
            </Space>
            <TaskSchemaFields typeId={step.type_id} index={index} />
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
function TaskSchemaFields({ typeId, index }: { typeId: string; index: number }) {
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

  if (!schema) {
    return <Typography.Text type="secondary">（无输入定义）</Typography.Text>;
  }
  // 输入值挂在 steps[index].input 路径下，与 Form 值树对齐。
  return <SchemaFormBridge schema={schema} prefix={["steps", index, "input"]} />;
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
