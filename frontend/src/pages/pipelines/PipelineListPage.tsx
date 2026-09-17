import { useQuery, useQueryClient } from "@tanstack/react-query";
import { App, Button, Space, Switch, Table, Tag, Typography } from "antd";
import type { TableColumnsType } from "antd";
import { useState } from "react";
import { useNavigate } from "react-router";
import type { components } from "@/api/schema";
import { client } from "@/api/client";
import { formatTime, unwrapData } from "@/api/helpers";

type PipelineSummary = components["schemas"]["PipelineSummaryResponse"];

/** 流水线列表（3.1）：概要、启用开关、删除确认。 */
export function PipelineListPage() {
  const navigate = useNavigate();
  const { message } = App.useApp();
  const queryClient = useQueryClient();
  const [page, setPage] = useState(1);

  const { data, isFetching } = useQuery({
    queryKey: ["pipelines", page],
    queryFn: async () => {
      const result = await client.GET("/api/pipelines", {
        params: { query: { page, page_size: 20 } },
      });
      return unwrapData<{ items: PipelineSummary[]; total: number }>(result);
    },
  });

  const toggleEnabled = async (row: PipelineSummary, enabled: boolean) => {
    // 启用开关走整体更新：先取完整定义再改 enabled（后端为整体替换语义）。
    const detail = await client.GET(`/api/pipelines/${row.id}`, {
      params: { path: { id: row.id } },
    });
    if (detail.error !== undefined || detail.data?.data === undefined) {
      message.error("读取流水线详情失败");
      return;
    }
    const full = detail.data.data;
    const result = await client.PUT(`/api/pipelines/${row.id}`, {
      params: { path: { id: row.id } },
      body: {
        name: full.name,
        description: full.description ?? null,
        enabled,
        steps: full.steps.map((step) => ({
          type_id: step.type_id,
          input: step.input,
          enabled: step.enabled,
        })),
      },
    });
    if (result.error !== undefined) {
      const body = result.error as { error?: { message?: string } };
      message.error(body?.error?.message ?? "更新失败");
      return;
    }
    message.success(enabled ? "已启用" : "已停用");
    await queryClient.invalidateQueries({ queryKey: ["pipelines"] });
  };

  const confirmDelete = (row: PipelineSummary) => {
    App.useApp().modal.confirm({
      title: "删除流水线？",
      content: `${row.name} 及其步骤、调度将一并删除。`,
      okButtonProps: { danger: true },
      okText: "删除",
      onOk: async () => {
        const result = await client.DELETE(`/api/pipelines/${row.id}`, {
          params: { path: { id: row.id } },
        });
        if (result.error !== undefined) {
          message.error("删除失败");
          return;
        }
        message.success("已删除");
        await queryClient.invalidateQueries({ queryKey: ["pipelines"] });
      },
    });
  };

  const columns: TableColumnsType<PipelineSummary> = [
    {
      title: "名称",
      dataIndex: "name",
      render: (name: string, row) => (
        <a onClick={() => navigate(`/pipelines/${row.id}`)}>{name}</a>
      ),
    },
    { title: "步骤数", dataIndex: "step_count", width: 100 },
    {
      title: "启用",
      dataIndex: "enabled",
      width: 100,
      render: (enabled: boolean, row) => (
        <Switch checked={enabled} onChange={(value) => toggleEnabled(row, value)} />
      ),
    },
    {
      title: "更新时间",
      dataIndex: "updated_at",
      width: 200,
      render: (value: string) => formatTime(value),
    },
    {
      title: "操作",
      width: 120,
      render: (_, row) => (
        <Space>
          <Button size="small" onClick={() => navigate(`/pipelines/${row.id}`)}>
            编辑
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
          流水线
        </Typography.Title>
        <Tag color={data?.items.some((row) => row.enabled) ? "processing" : "default"}>
          {data?.items.filter((row) => row.enabled).length ?? 0} 条启用中
        </Tag>
      </Space>
      <div style={{ marginBottom: 16 }}>
        <Button type="primary" onClick={() => navigate("/pipelines/new")}>
          新建流水线
        </Button>
      </div>
      <Table<PipelineSummary>
        rowKey="id"
        loading={isFetching}
        columns={columns}
        dataSource={data?.items ?? []}
        pagination={{
          current: page,
          pageSize: 20,
          total: data?.total ?? 0,
          onChange: setPage,
          showTotal: (total) => `共 ${total} 条`,
        }}
      />
    </div>
  );
}
