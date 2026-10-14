import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Alert, Card, Descriptions, Space, Spin, Tag, Typography } from "antd";
import type { components } from "@/api/schema";
import { useParams } from "react-router";
import { client } from "@/api/client";
import { formatTime, unwrapData } from "@/api/helpers";
import { useEffect, useState } from "react";
import { PageHeader } from "@/components/PageHeader";
import { useAuthStore } from "@/auth/store";

type HistoryDetail = components["schemas"]["HistoryResponse"];

interface LogRow {
  step_index: number;
  level: string;
  message: string;
  created_at: string;
  output?: unknown;
}

function tencentOutput(output: unknown): { certIds: string[]; skipped: boolean } | null {
  if (!output || typeof output !== "object" || Array.isArray(output)) return null;
  const obj = output as Record<string, unknown>;
  if (!("cert_ids" in obj)) return null;
  const certIds = Array.isArray(obj.cert_ids)
    ? obj.cert_ids.filter((v): v is string => typeof v === "string")
    : [];
  const skipped = obj.skipped_write === true;
  return { certIds, skipped };
}

const LEVEL_COLOR: Record<string, string> = {
  info: "#1677ff",
  warn: "#faad14",
  error: "#ff4d4f",
};

/** 运行详情：通过 SSE 实时订阅增量日志与终态，失败时降级单次拉取。 */
export function HistoryDetailPage() {
  const { id } = useParams();
  const historyId = Number(id);
  const queryClient = useQueryClient();
  const [fallbackWarning, setFallbackWarning] = useState(false);

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
  });

  // SSE 实时推送订阅
  useEffect(() => {
    if (!Number.isFinite(historyId) || history?.status !== "running") {
      return;
    }

    if (typeof window === "undefined" || typeof window.EventSource === "undefined") {
      setFallbackWarning(true);
      void queryClient.refetchQueries({ queryKey: ["history-logs", historyId] });
      return;
    }

    const token = useAuthStore.getState().token;
    const url = token
      ? `/api/histories/${historyId}/events?token=${encodeURIComponent(token)}`
      : `/api/histories/${historyId}/events`;

    const es = new EventSource(url);

    es.addEventListener("started", (event) => {
      try {
        const data = JSON.parse(event.data) as Partial<HistoryDetail>;
        queryClient.setQueryData<HistoryDetail>(["history", historyId], (prev) =>
          prev
            ? {
                ...prev,
                started_at: data.started_at ?? prev.started_at,
                trigger_source: data.trigger_source ?? prev.trigger_source,
              }
            : prev,
        );
      } catch {
        // 忽略解析异常
      }
    });

    es.addEventListener("log", (event) => {
      try {
        const data = JSON.parse(event.data) as LogRow;
        queryClient.setQueryData<LogRow[]>(["history-logs", historyId], (prev) => {
          const list = prev ? [...prev] : [];
          // REST 全量与 SSE 重放可能重复投递同一条日志（如启动日志），按三元组查重
          const exists = list.some(
            (row) =>
              row.step_index === data.step_index &&
              row.message === data.message &&
              row.created_at === data.created_at,
          );
          if (exists) return list;
          list.push({
            step_index: data.step_index,
            level: data.level,
            message: data.message,
            created_at: data.created_at,
            output: data.output,
          });
          return list;
        });
      } catch {
        // 忽略解析异常
      }
    });

    es.addEventListener("succeeded", (event) => {
      try {
        const data = JSON.parse(event.data) as { finished_at?: string };
        queryClient.setQueryData<HistoryDetail>(["history", historyId], (prev) =>
          prev
            ? {
                ...prev,
                status: "success",
                finished_at: data.finished_at ?? new Date().toISOString(),
                error_message: null,
              }
            : prev,
        );
      } catch {
        // 忽略解析异常
      }
      es.close();
      void queryClient.invalidateQueries({ queryKey: ["histories"] });
    });

    es.addEventListener("failed", (event) => {
      try {
        const data = JSON.parse(event.data) as { finished_at?: string; error_message?: string };
        queryClient.setQueryData<HistoryDetail>(["history", historyId], (prev) =>
          prev
            ? {
                ...prev,
                status: "failed",
                finished_at: data.finished_at ?? new Date().toISOString(),
                error_message: data.error_message ?? null,
              }
            : prev,
        );
      } catch {
        // 忽略解析异常
      }
      es.close();
      void queryClient.invalidateQueries({ queryKey: ["histories"] });
    });

    es.onerror = () => {
      es.close();
      setFallbackWarning(true);
      void queryClient.refetchQueries({ queryKey: ["history-logs", historyId] });
      void queryClient.refetchQueries({ queryKey: ["history", historyId] });
    };

    return () => {
      es.close();
    };
  }, [historyId, history?.status, queryClient]);

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

      {fallbackWarning && (
        <Alert
          type="warning"
          showIcon
          message="实时更新不可用，已回退为拉取模式"
          style={{ marginBottom: 16 }}
          closable
          onClose={() => setFallbackWarning(false)}
        />
      )}

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
          // 步骤错误（如 cert.apply 跳过注入失败）由后端产出领域文案：
          // 含证书标识、已尝试路径与可操作指引，不含裸 OS 错误，前端原样展示即可。
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
            {stepLogs.map((log, index) => {
              const structured = tencentOutput((log as LogRow).output);
              const hasCertIds = structured !== null && structured.certIds.length > 0;
              const skippedWithEmpty =
                structured !== null && structured.skipped && structured.certIds.length === 0;
              return (
                <div key={index}>
                  <Typography.Text
                    style={{ color: LEVEL_COLOR[log.level] ?? undefined, fontFamily: "monospace" }}
                  >
                    [{formatTime(log.created_at)}] {log.message}
                  </Typography.Text>
                  {hasCertIds && (
                    <div style={{ marginTop: 6 }}>
                      <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                        腾讯云证书 ID
                      </Typography.Text>
                      <Space size={[6, 6]} wrap style={{ marginTop: 4 }}>
                        {structured!.certIds.map((certId) => (
                          <Tag key={certId} color="geekblue">
                            <Typography.Text copyable={{ text: certId }} style={{ fontSize: 12 }}>
                              {certId}
                            </Typography.Text>
                          </Tag>
                        ))}
                      </Space>
                    </div>
                  )}
                  {skippedWithEmpty && (
                    <div style={{ marginTop: 6 }}>
                      <Tag>指纹一致，已跳过写入</Tag>
                      <Typography.Text type="secondary" style={{ fontSize: 12, marginLeft: 6 }}>
                        无需上传，云端证书保持不变
                      </Typography.Text>
                    </div>
                  )}
                </div>
              );
            })}
          </Space>
        </Card>
      ))}
    </div>
  );
}
