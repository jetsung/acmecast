import { useQuery, useQueryClient } from "@tanstack/react-query";
import { App, Button, Card, Checkbox, Form, Input, Modal, Select, Space, Table, Tag, Typography } from "antd";
import type { TableColumnsType } from "antd";
import { useEffect, useState } from "react";
import type { components } from "@/api/schema";
import { ApiError, client } from "@/api/client";
import { formatTime, unwrapData } from "@/api/helpers";
import { SchemaForm } from "@/components/SchemaForm";
import { PageHeader } from "@/components/PageHeader";

type CredentialRow = components["schemas"]["CredentialResponse"];

// 与后端 acmecast_store::capability::CONTROLLED_TYPES 保持一致：
// 仅这三类凭据声明并持久化能力（dns / deploy）。
const CAPABILITY_CONTROLLED_TYPES = ["cloud.tencent", "cloud.cloudflare", "cloud.aliyun"];
const CAPABILITY_OPTIONS = [
  { label: "DNS", value: "dns" },
  { label: "部署", value: "deploy" },
];
const DEFAULT_CAPABILITIES = ["dns", "deploy"];

function isCapabilityControlled(typeId: string | null | undefined): boolean {
  return typeId !== null && typeId !== undefined && CAPABILITY_CONTROLLED_TYPES.includes(typeId);
}

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
  const [pageSize, setPageSize] = useState(15);
  const [editorOpen, setEditorOpen] = useState(false);
  const [editingId, setEditingId] = useState<number | null>(null);
  const [selectedType, setSelectedType] = useState<string | null>(null);
  const [testing, setTesting] = useState<number | null>(null);

  const { data: credentials, isFetching } = useQuery({
    queryKey: ["credentials", page, pageSize],
    queryFn: async () => {
      const result = await client.GET("/api/credentials", {
        params: { query: { page, page_size: pageSize } },
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
    { title: "ID", dataIndex: "id", width: 90, sorter: false },
    { title: "名称", dataIndex: "name" },
    { title: "类型", dataIndex: "type_id", render: (value: string) => <Tag>{value}</Tag> },
    {
      title: "可用权限",
      dataIndex: "capabilities",
      width: 140,
      render: (_: unknown, row) => {
        // 仅受控类型展示能力；非受控或能力为空显示占位。
        if (!isCapabilityControlled(row.type_id) || !row.capabilities?.length) {
          return <Typography.Text type="secondary">—</Typography.Text>;
        }
        // 字符串形式「DNS、部署」随文本流居左，不用标签并排（视觉上易被当作居中）。
        return (
          <Typography.Text>
            {row.capabilities.map((capability) => (capability === "dns" ? "DNS" : "部署")).join("、")}
          </Typography.Text>
        );
      },
    },
    {
      title: "代理",
      dataIndex: "proxy_mode",
      width: 120,
      render: (value: string) => {
        // 代理模式中文标签（列表即可辨认出网代理方式，不必逐条打开编辑弹窗）；
        // 与「可用权限」列一致用纯文本左对齐，不用 Tag。
        const label = value === "inherit" ? "继承全局" : value === "custom" ? "自定义" : value === "direct" ? "直连" : null;
        if (label === null) {
          return <Typography.Text type="secondary">—</Typography.Text>;
        }
        return <Typography.Text>{label}</Typography.Text>;
      },
    },
    {
      title: "备注",
      dataIndex: "remark",
      render: (value: string | null | undefined) =>
        value && value.trim() !== "" ? value : <Typography.Text type="secondary">—</Typography.Text>,
    },
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
          pageSize,
          total: credentials?.total ?? 0,
          onChange: (newPage, newPageSize) => {
            if (newPageSize !== pageSize) {
              setPage(1);
              setPageSize(newPageSize);
            } else {
              setPage(newPage);
            }
          },
            showSizeChanger: true,
            pageSizeOptions: [10, 15, 20, 50, 100],
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
  if (!schema || type.type_id !== "deploy.ssh") return type.schema;
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
  const { data: detail, isFetching: detailLoading } = useQuery({
    queryKey: ["credential-detail", editingId],
    queryFn: async () => {
      if (editingId === null) throw new Error("编辑目标不存在");
      const result = await client.GET(`/api/credentials/${editingId}`, {
        params: { path: { id: editingId } },
      });
      // schema 重新生成后 fields 变成 unknown（字段结构由 type_id 决定），
      // 这里只消费「名字/类型 + 一组待回填的键值」，以 unknown 中转。
      const detail = (await unwrapData<{
        id: number;
        name: string;
        type_id: string;
        fields: unknown;
        remark?: string | null;
        capabilities?: string[];
      }>(result)) as {
        id: number;
        name: string;
        type_id: string;
        fields: Record<string, unknown>;
        remark?: string | null;
        capabilities?: string[];
      };
      return detail;
    },
    enabled: open && editingId !== null,
  });

  // 切换编辑目标时立即重置表单，避免上一条记录的旧值在新详情到达前闪现。
  useEffect(() => {
    if (!open) return;
    form.resetFields();
  }, [open, editingId, form]);

  // 仅当详情对应当前 editingId 时回填；过期请求的响应不得覆盖当前弹窗。
  useEffect(() => {
    if (!open) return;
    if (editingId === null) return;
    if (detailLoading) return;
    if (detail && detail.id === editingId) {
      form.setFieldsValue({
        name: detail.name,
        type_id: detail.type_id,
        remark: detail.remark ?? "",
        fields: detail.fields ?? {},
        // 受控类型回填现值；缺省（旧数据）按双能力处理。
        capabilities: isCapabilityControlled(detail.type_id)
          ? (detail.capabilities?.length ? detail.capabilities : DEFAULT_CAPABILITIES)
          : undefined,
      });
    }
  }, [open, detail, detailLoading, editingId, form]);

  const onTypeSelect = (typeId: string) => {
    onTypeChange(typeId);
    // 类型切换清空字段值，避免上一类型的残留值混入新类型；
    // 受控类型把能力恢复为默认双选，非受控清掉避免脏值提交。
    form.setFieldsValue({
      name: form.getFieldValue("name"),
      fields: {},
      capabilities: isCapabilityControlled(typeId) ? DEFAULT_CAPABILITIES : undefined,
    });
  };

  const onSave = async (values: {
    name: string;
    remark?: string;
    fields: Record<string, unknown>;
    capabilities?: string[];
  }) => {
    setSaving(true);
    try {
      const body = {
        name: values.name,
        type_id: selectedType ?? "",
        fields: values.fields ?? {},
        ...(values.remark && values.remark.trim() !== "" ? { remark: values.remark.trim() } : {}),
        // 仅受控类型提交能力字段；非受控类型由后端忽略，这里也不传。
        ...(isCapabilityControlled(selectedType) && values.capabilities
          ? { capabilities: values.capabilities }
          : {}),
      } as {
        name: string;
        type_id: string;
        fields: unknown;
        remark?: string | null;
        capabilities?: string[];
      };
      const result =
        editingId === null
          ? await client.POST("/api/credentials", { body })
          : await client.PUT(`/api/credentials/${editingId}`, {
              params: { path: { id: editingId } },
              body,
            } as never);
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

  // 编辑模式下详情加载期间禁用保存，避免用空表单或陈旧数据提交。
  const okDisabled = editingId !== null && detailLoading;

  return (
    <Modal
      open={open}
      title={editingId === null ? "新建凭据" : "编辑凭据"}
      width={720}
      onCancel={onClose}
      onOk={() => form.submit()}
      okButtonProps={{ disabled: okDisabled }}
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
        {/* 受控类型（tencent/cloudflare/aliyun）的能力多选：外层表单项、
            独占一行（vertical 布局天然整行），不进 SchemaForm。 */}
        {isCapabilityControlled(selectedType) && (
          <Form.Item
            name="capabilities"
            label="可用权限"
            rules={[
              { required: true, message: "请选择可用权限" },
              {
                validator: (_rule, value: string[] | undefined) =>
                  value && value.length > 0
                    ? Promise.resolve()
                    : Promise.reject(new Error("至少选择一项可用权限")),
              },
            ]}
          >
            <Checkbox.Group options={CAPABILITY_OPTIONS} />
          </Form.Item>
        )}
        <Form.Item name="remark" label="备注">
          <Input placeholder="可选：如「生产 DNS」「测试 CLB」，用于区分多密钥场景" maxLength={255} />
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
