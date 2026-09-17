import { useQuery, useQueryClient } from "@tanstack/react-query";
import { App, Button, Card, Form, Input, Modal, Select, Space, Table, Tag, Typography } from "antd";
import type { TableColumnsType } from "antd";
import { useState } from "react";
import type { components } from "@/api/schema";
import { ApiError, client } from "@/api/client";
import { formatTime, unwrapData } from "@/api/helpers";
import { SchemaForm } from "@/components/SchemaForm";

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
      <Space style={{ marginBottom: 16 }}>
        <Typography.Title level={4} style={{ margin: 0 }}>
          凭据
        </Typography.Title>
      </Space>
      <div style={{ marginBottom: 16 }}>
        <Button type="primary" onClick={() => openEditor(null)}>
          新建凭据
        </Button>
      </div>
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
                  <SchemaForm schema={currentType.schema} form={form} namePrefix={["fields"]} />
                </Card>
              )}
            </Form.Item>
          </>
        )}
      </Form>
    </Modal>
  );
}
