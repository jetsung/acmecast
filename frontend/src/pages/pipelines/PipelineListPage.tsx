import { useQuery, useQueryClient } from "@tanstack/react-query";
import { App, Button, Space, Switch, Table, Tag } from "antd";
import type { TableColumnsType } from "antd";
import { useState } from "react";
import { useNavigate } from "react-router";
import type { components } from "@/api/schema";
import { client } from "@/api/client";
import { formatTime, unwrapData } from "@/api/helpers";
import { PageHeader } from "@/components/PageHeader";
import { Panel } from "@/components/Panel";

type PipelineSummary = components["schemas"]["PipelineSummaryResponse"];

/** 流水线列表（3.1）：概要、启用开关、立即运行、删除确认。 */
export function PipelineListPage() {
  const navigate = useNavigate();
  const { message } = App.useApp();
  const queryClient = useQueryClient();
  const [page, setPage] = useState(1);
  const [runningId, setRunningId] = useState<number | null>(null);

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

  // 手动触发一次运行。后端异步执行并立即返回运行历史标识，这里直接跳到
  // 运行详情页——它是轮询式的，进去就能看到 running 变成 success。
  const runNow = async (row: PipelineSummary) => {
    setRunningId(row.id);
    try {
      const result = await client.POST(`/api/pipelines/${row.id}/run`, {
        params: { path: { id: row.id } },
      });
      if (result.error !== undefined) {
        const body = result.error as { error?: { message?: string } };
        message.error(body?.error?.message ?? "触发运行失败");
        return;
      }
      const historyId = result.data?.data?.history_id;
      message.success(
        historyId === undefined ? "已开始运行" : `已开始运行，运行记录 #${historyId}`,
      );
      if (historyId !== undefined) {
        navigate(`/histories/${historyId}`);
      }
    } finally {
      setRunningId(null);
    }
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
      width: 220,
      render: (_, row) => (
        <Space size={4}>
          <Button size="small" type="link" loading={runningId === row.id} onClick={() => runNow(row)}>
            立即运行
          </Button>
          <Button size="small" type="link" onClick={() => navigate(`/pipelines/${row.id}`)}>
            编辑
          </Button>
          <Button size="small" type="link" danger onClick={() => confirmDelete(row)}>
            删除
          </Button>
        </Space>
      ),
    },
  ];

  return (
    <div>
      <PageHeader
        title="流水线"
        subtitle={
          <Tag color={data?.items.some((row) => row.enabled) ? "processing" : "default"}>
            {data?.items.filter((row) => row.enabled).length ?? 0} 条启用中
          </Tag>
        }
        description="证书签发与部署的自动化流程"
        extra={
          <Button type="primary" onClick={() => navigate("/pipelines/new")}>
            新建流水线
          </Button>
        }
      />
      <Panel>
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
      </Panel>
    </div>
  );
}
