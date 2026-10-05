import { useQuery } from "@tanstack/react-query";
import { Alert, Select, Table, Tag, Typography } from "antd";
import type { TableColumnsType } from "antd";
import { useState } from "react";
import { useNavigate } from "react-router";
import type { components } from "@/api/schema";
import { client } from "@/api/client";
import { formatTime, unwrapData } from "@/api/helpers";
import { PageHeader } from "@/components/PageHeader";
import { Panel } from "@/components/Panel";

type HistoryRow = components["schemas"]["HistoryResponse"];

const SOURCE_LABEL: Record<string, { label: string; color: string }> = {
  manual: { label: "手动", color: "blue" },
  cron: { label: "定时", color: "purple" },
  renewal: { label: "续期", color: "cyan" },
};

const STATUS_TAG: Record<string, { label: string; color: string }> = {
  running: { label: "运行中", color: "processing" },
  success: { label: "成功", color: "success" },
  failed: { label: "失败", color: "error" },
};

/** 运行历史列表（2.3）：按流水线过滤、状态筛选、分页。 */
export function HistoryListPage() {
  const navigate = useNavigate();
  const [pipelineId, setPipelineId] = useState<number | undefined>();
  const [page, setPage] = useState(1);

  const { data, isFetching, error } = useQuery({
    queryKey: ["histories", pipelineId, page],
    queryFn: async () => {
      const result = await client.GET("/api/histories", {
        params: {
          query: { pipeline_id: pipelineId, page, page_size: 20 },
        },
      });
      return unwrapData<{ items: HistoryRow[]; total: number }>(result);
    },
  });

  const columns: TableColumnsType<HistoryRow> = [
    { title: "ID", dataIndex: "id", width: 80 },
    { title: "流水线", dataIndex: "pipeline_id", width: 100 },
    {
      title: "触发来源",
      dataIndex: "trigger_source",
      width: 100,
      render: (source: string) => {
        const meta = SOURCE_LABEL[source] ?? { label: source, color: "default" };
        return <Tag color={meta.color}>{meta.label}</Tag>;
      },
    },
    {
      title: "状态",
      dataIndex: "status",
      width: 100,
      render: (status: string) => {
        const meta = STATUS_TAG[status] ?? { label: status, color: "default" };
        return <Tag color={meta.color}>{meta.label}</Tag>;
      },
    },
    {
      title: "开始时间",
      dataIndex: "started_at",
      width: 200,
      render: (value: string) => formatTime(value),
    },
    {
      title: "结束时间",
      dataIndex: "finished_at",
      width: 200,
      render: (value: string | null) => formatTime(value),
    },
    {
      title: "失败原因",
      dataIndex: "error_message",
      ellipsis: true,
      render: (value: string | null) =>
        value ? <Typography.Text type="danger">{value}</Typography.Text> : "-",
    },
  ];

  return (
    <div>
      <PageHeader
        title="运行历史"
        description="流水线每次执行的审计与结果"
        extra={
          <Select
            allowClear
            placeholder="按流水线过滤"
            style={{ width: 200 }}
            onChange={(value) => {
              setPipelineId(value);
              setPage(1);
            }}
            options={(data?.items ?? [])
              .map((item) => item.pipeline_id)
              .filter((value, index, all) => all.indexOf(value) === index)
              .map((value) => ({ value, label: `流水线 #${value}` }))}
          />
        }
      />
      {error && (
        // 查询失败时把原因摆出来：吞掉错误会让「没数据」和「没加载成功」无法区分。
        <Alert
          type="error"
          showIcon
          closable
          message="运行历史加载失败"
          description={error.message}
          style={{ marginBottom: 16 }}
        />
      )}
      <Panel>
        <Table<HistoryRow>
          rowKey="id"
          loading={isFetching}
          columns={columns}
          dataSource={data?.items ?? []}
          onRow={(row) => ({
            onClick: () => navigate(`/histories/${row.id}`),
            style: { cursor: "pointer" },
          })}
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
