import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Alert, Card, Descriptions, Space, Spin, Tag, Typography } from "antd";
import type { components } from "@/api/schema";
import { useParams } from "react-router";
import { client } from "@/api/client";
import { formatTime, unwrapData } from "@/api/helpers";
import { useEffect } from "react";
import { PageHeader } from "@/components/PageHeader";

type HistoryDetail = components["schemas"]["HistoryResponse"];

interface LogRow {
  step_index: number;
  level: string;
  message: string;
  created_at: string;
}

const LEVEL_COLOR: Record<string, string> = {
  info: "#1677ff",
  warn: "#faad14",
  error: "#ff4d4f",
};

/** 运行详情（2.3）：running 状态 5 秒轮询至终态；日志按步骤分组着色。 */
export function HistoryDetailPage() {
  const { id } = useParams();
  const historyId = Number(id);
  const queryClient = useQueryClient();

  const { data: history, error: historyError } = useQuery({
    queryKey: ["history", historyId],
    queryFn: async () => {
      const result = await client.GET(`/api/histories/${historyId}`, {
        params: { path: { id: historyId } },
      });
      if (result.error !== undefined) {
        return undefined;
      }
      return await unwrapData<HistoryDetail>(result);
    },
    enabled: Number.isFinite(historyId),
    // running 时轮询终态，与日志轮询同一节奏：手动触发后跳进来，状态会自行
    // 从 running 变为 success/failed，不必手动刷新。
    refetchInterval: (query) => (query.state.data?.status === "running" ? 5000 : false),
  });

  const { data: logs, isLoading } = useQuery({
    queryKey: ["history-logs", historyId],
    queryFn: async () => {
      const result = await client.GET(`/api/histories/${historyId}/logs`, {
        params: { path: { id: historyId } },
      });
      return unwrapData<LogRow[]>(result);
    },
    enabled: Number.isFinite(historyId),
    // running 时轮询：终态即停（design D6）。
    refetchInterval: history?.status === "running" ? 5000 : false,
  });

  // 终态到达后让列表缓存失效一次，保证列表页状态同步。
  useEffect(() => {
    if (history && history.status !== "running") {
      void queryClient.invalidateQueries({ queryKey: ["histories"] });
    }
  }, [history?.status, history, queryClient]);

  if (historyError) {
    // 失败要说明白：吞掉错误只留 Spin 的话，「查不到」和「还在查」看起来一样。
    return (
      <Alert
        type="error"
        showIcon
        message="运行详情加载失败"
        description={historyError.message}
      />
    );
  }

  if (!history || isLoading) {
    return <Spin />;
  }

  const steps = new Map<number, LogRow[]>();
  for (const log of logs ?? []) {
    const list = steps.get(log.step_index) ?? [];
    list.push(log);
    steps.set(log.step_index, list);
  }

  return (
    <div>
      <PageHeader
        title={`运行 #${history.id}`}
        subtitle={
          <Tag
            color={history.status === "failed" ? "error" : history.status === "running" ? "processing" : "success"}
          >
            {history.status}
          </Tag>
        }
        description={`流水线 #${history.pipeline_id} · 触发来源 ${history.trigger_source}`}
      />

      <Card style={{ marginBottom: 24 }}>
        <Descriptions bordered size="small" column={2}>
          <Descriptions.Item label="流水线">#{history.pipeline_id}</Descriptions.Item>
          <Descriptions.Item label="触发来源">{history.trigger_source}</Descriptions.Item>
          <Descriptions.Item label="开始">{formatTime(history.started_at)}</Descriptions.Item>
          <Descriptions.Item label="结束">{formatTime(history.finished_at)}</Descriptions.Item>
        </Descriptions>
      </Card>

      {history.error_message && (
        <Alert
          type="error"
          showIcon
          message="失败原因"
          description={history.error_message}
          style={{ marginBottom: 24 }}
        />
      )}

      {[...steps.entries()].map(([stepIndex, stepLogs]) => (
        <Card
          key={stepIndex}
          size="small"
          title={`步骤 ${stepIndex + 1}`}
          style={{ marginBottom: 16 }}
        >
          <Space direction="vertical" style={{ width: "100%" }}>
            {stepLogs.map((log, index) => (
              <Typography.Text
                key={index}
                style={{ color: LEVEL_COLOR[log.level] ?? undefined, fontFamily: "monospace" }}
              >
                [{formatTime(log.created_at)}] {log.message}
              </Typography.Text>
            ))}
          </Space>
        </Card>
      ))}
    </div>
  );
}
