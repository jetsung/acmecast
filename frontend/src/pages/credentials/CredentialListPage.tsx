import { useQuery, useQueryClient } from "@tanstack/react-query";
import { App, Button, Card, Form, Input, Modal, Select, Space, Table, Tag, Typography } from "antd";
import type { TableColumnsType } from "antd";
import { useEffect, useState } from "react";
import type { components } from "@/api/schema";
import { ApiError, client } from "@/api/client";
import { formatTime, unwrapData } from "@/api/helpers";
import { SchemaForm } from "@/components/SchemaForm";
import { PageHeader } from "@/components/PageHeader";

type CredentialRow = components["schemas"]["CredentialResponse"];

interface CredentialTypeRow {
  type_id: string;
  display_name: string;
  schema: unknown;
}

interface ReferenceRow {
  pipeline_id: number;
  pipeline_name: string;
  step_order: number;
}

/** 凭据管理（2.4）：列表、创建/编辑（schema 驱动表单）、连通性测试、引用检查删除。 */
export function CredentialListPage() {
  const { message } = App.useApp();
  const queryClient = useQueryClient();
  const [page, setPage] = useState(1);
  const [editorOpen, setEditorOpen] = useState(false);
  const [editingId, setEditingId] = useState<number | null>(null);
  const [selectedType, setSelectedType] = useState<string | null>(null);
  const [testing, setTesting] = useState<number | null>(null);

  const { data: credentials, isFetching } = useQuery({
    queryKey: ["credentials", page],
    queryFn: async () => {
      const result = await client.GET("/api/credentials", {
        params: { query: { page, page_size: 20 } },
      });
      return unwrapData<{ items: CredentialRow[]; total: number }>(result);
    },
  });

  const { data: types } = useQuery({
    queryKey: ["credential-types"],
    queryFn: async () => {
      const result = await client.GET("/api/credential-types", {
        cache: "no-store",
      });
      return unwrapData<CredentialTypeRow[]>(result);
    },
  });

  const openEditor = (row: CredentialRow | null) => {
    setEditingId(row?.id ?? null);
    setSelectedType(row?.type_id ?? null);
    setEditorOpen(true);
  };

  const confirmDelete = (row: CredentialRow) => {
    Modal.confirm({
      title: "删除凭据？",
      content: `${row.name}（${row.type_id}）删除后无法恢复。`,
      okButtonProps: { danger: true },
      okText: "删除",
      onOk: async () => {
        const result = await client.DELETE(`/api/credentials/${row.id}`, {
          params: { path: { id: row.id } },
        });
        if (result.error !== undefined) {
          // 引用检查失败（409）：展示引用它的流水线清单。
          const body = result.error as {
            error?: {
              code?: string;
              message?: string;
              referenced_by?: ReferenceRow[];
            };
          };
          const references = body?.error?.referenced_by;
          if (references && references.length > 0) {
            Modal.warning({
              title: "凭据仍被流水线引用，无法删除",
              content: (
                <ul>
                  {references.map((ref) => (
                    <li key={`${ref.pipeline_id}-${ref.step_order}`}>
                      {ref.pipeline_name}（步骤 {ref.step_order + 1}）
                    </li>
                  ))}
                </ul>
              ),
            });
            return;
          }
          message.error(body?.error?.message ?? "删除失败");
          return;
        }
        message.success("凭据已删除");
        await queryClient.invalidateQueries({ queryKey: ["credentials"] });
      },
    });
  };

  const runConnectivityTest = async (row: CredentialRow) => {
    setTesting(row.id);
    try {
      const result = await client.POST(`/api/credentials/${row.id}/test`, {
        params: { path: { id: row.id } },
      });
      const body = await unwrapData<{ status: string; reason?: string | null }>(result);
      if (body.status === "ok") {
        message.success("凭据可用");
      } else {
        message.warning(`不可用：${body.reason ?? "原因未提供"}`);
      }
    } catch (error) {
      message.error(error instanceof ApiError ? error.message : "测试失败");
    } finally {
      setTesting(null);
    }
  };

  const columns: TableColumnsType<CredentialRow> = [
    { title: "名称", dataIndex: "name" },
    { title: "类型", dataIndex: "type_id", render: (value: string) => <Tag>{value}</Tag> },
    {
      title: "更新时间",
      dataIndex: "updated_at",
      width: 200,
      render: (value: string) => formatTime(value),
    },
    {
      title: "操作",
      width: 260,
      render: (_, row) => (
        <Space>
          <Button size="small" onClick={() => openEditor(row)}>
            编辑
          </Button>
          <Button
            size="small"
            loading={testing === row.id}
            onClick={() => runConnectivityTest(row)}
          >
            测试
          </Button>
          <Button size="small" danger onClick={() => confirmDelete(row)}>
            删除
          </Button>
        </Space>
      ),
    },
  ];

  return (
    <div>
      <PageHeader
        title="凭据"
        description="DNS 提供商、SSH 主机等外部系统的接入凭据"
        extra={
          <Button type="primary" onClick={() => openEditor(null)}>
            新建凭据
          </Button>
        }
      />
      {/* 表格不包 Panel 容器：antd Table 在嵌套容器中会让 jsdom 单测
          （编辑回填用例）多出约 3.5s 测量开销、卡 5s 默认超时；
          Table 自带白底圆角，裸放观感一致。 */}
      <Table<CredentialRow>
        rowKey="id"
        loading={isFetching}
        columns={columns}
        dataSource={credentials?.items ?? []}
        pagination={{
          current: page,
          pageSize: 20,
          total: credentials?.total ?? 0,
          onChange: setPage,
          showTotal: (total) => `共 ${total} 条`,
        }}
      />

      <CredentialEditorModal
        open={editorOpen}
        editingId={editingId}
        types={types ?? []}
        selectedType={selectedType}
        onTypeChange={setSelectedType}
        onClose={() => setEditorOpen(false)}
      />
    </div>
  );
}

/**
 * SSH 主机凭据的字段渲染顺序：权限位成对在前，连接信息成对随后，认证材料
 * 最后（私钥是多行文本域，独占一行收尾）。仅声明顺序，不改字段集。
 */
const SSH_HOST_FIELD_ORDER = [
  "cert_mode",
  "key_mode",
  "host",
  "port",
  "user",
  "password",
  "private_key",
];

/**
 * 按凭据类型给字段 schema 注入渲染顺序：SSH 主机类型的 schema 键序由后端
 * 结构体决定，与录入时的阅读顺序不一致，这里显式声明。
 */
function withTypeFieldOrder(type: CredentialTypeRow): unknown {
  const schema = type.schema as Record<string, unknown> | undefined;
  if (!schema || type.type_id !== "ssh") return type.schema;
  return { ...schema, "x-field-order": SSH_HOST_FIELD_ORDER };
}

function CredentialEditorModal({
  open,
  editingId,
  types,
  selectedType,
  onTypeChange,
  onClose,
}: {
  open: boolean;
  editingId: number | null;
  types: CredentialTypeRow[];
  selectedType: string | null;
  onTypeChange: (value: string | null) => void;
  onClose: () => void;
}) {
  const { message } = App.useApp();
  const queryClient = useQueryClient();
  const [form] = Form.useForm();
  const [saving, setSaving] = useState(false);

  const currentType = types.find((item) => item.type_id === selectedType);

  // 编辑要回填本条记录的现值：PUT 是整体替换语义，空表单直接保存会把
  // 已存的密钥清成空。字段值只有详情接口带回（列表接口不带）。
  const { data: detail } = useQuery({
    queryKey: ["credential-detail", editingId],
    queryFn: async () => {
      if (editingId === null) throw new Error("编辑目标不存在");
      const result = await client.GET(`/api/credentials/${editingId}`, {
        params: { path: { id: editingId } },
      });
      // schema 重新生成后 fields 变成 unknown（字段结构由 type_id 决定），
      // 这里只消费「名字/类型 + 一组待回填的键值」，以 unknown 中转。
      const detail = (await unwrapData<{ name: string; type_id: string; fields: unknown }>(
        result,
      )) as { name: string; type_id: string; fields: Record<string, unknown> };
      return detail;
    },
    enabled: open && editingId !== null,
  });

  useEffect(() => {
    if (!open) return;
    if (detail) {
      form.setFieldsValue({
        name: detail.name,
        type_id: detail.type_id,
        fields: detail.fields ?? {},
      });
    } else if (editingId === null) {
      form.resetFields();
    }
  }, [open, detail, editingId, form]);

  const onTypeSelect = (typeId: string) => {
    onTypeChange(typeId);
    // 类型切换清空字段值，避免上一类型的残留值混入新类型。
    form.setFieldsValue({ name: form.getFieldValue("name"), fields: {} });
  };

  const onSave = async (values: { name: string; fields: Record<string, unknown> }) => {
    setSaving(true);
    try {
      const body = {
        name: values.name,
        type_id: selectedType ?? "",
        fields: values.fields ?? {},
      };
      const result =
        editingId === null
          ? await client.POST("/api/credentials", { body })
          : await client.PUT(`/api/credentials/${editingId}`, {
              params: { path: { id: editingId } },
              body,
            });
      if (result.error !== undefined) {
        const body = result.error as { error?: { message?: string; field?: string } };
        if (body?.error?.field) {
          form.setFields([
            { name: ["fields", body.error.field], errors: [body.error.message ?? "不合法"] },
          ]);
          return;
        }
        message.error(body?.error?.message ?? "保存失败");
        return;
      }
      message.success("凭据已保存");
      await queryClient.invalidateQueries({ queryKey: ["credentials"] });
      onClose();
    } finally {
      setSaving(false);
    }
  };

  return (
    <Modal
      open={open}
      title={editingId === null ? "新建凭据" : "编辑凭据"}
      width={720}
      onCancel={onClose}
      onOk={() => form.submit()}
      confirmLoading={saving}
      destroyOnHidden
    >
      <Form form={form} layout="vertical" onFinish={onSave}>
        <Form.Item
          name="name"
          label="名称"
          rules={[{ required: true, message: "请输入凭据名称" }]}
        >
          <Input placeholder="例如：主域名 DNS 凭据" />
        </Form.Item>
        <Form.Item
          name="type_id"
          label="类型"
          rules={[{ required: true, message: "请选择凭据类型" }]}
        >
          <Select
            disabled={editingId !== null}
            placeholder="选择类型"
            options={types.map((item) => ({
              value: item.type_id,
              label: item.display_name,
            }))}
            onChange={(value) => onTypeSelect(value)}
          />
        </Form.Item>

        {currentType && (
          <>
            <Typography.Text type="secondary">字段（由类型定义驱动）</Typography.Text>
            <Form.Item
              noStyle
              shouldUpdate={(prev, next) => prev.type_id !== next.type_id}
            >
              {() => (
                <Card size="small" style={{ marginTop: 8 }}>
                  <SchemaForm
                    schema={withTypeFieldOrder(currentType)}
                    form={form}
                    namePrefix={["fields"]}
                  />
                </Card>
              )}
            </Form.Item>
          </>
        )}
      </Form>
    </Modal>
  );
}
