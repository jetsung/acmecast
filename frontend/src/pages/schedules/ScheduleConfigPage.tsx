import { useQuery, useQueryClient } from "@tanstack/react-query";
import { App, Button, Card, Form, Input, Select, Space, Switch, Tag, Typography } from "antd";
import { CronExpressionParser } from "cron-parser";
import { useState } from "react";
import type { components } from "@/api/schema";
import { client } from "@/api/client";
import { formatTime, unwrapData } from "@/api/helpers";

type ScheduleRow = components["schemas"]["ScheduleResponse"];

/** cron 前置校验（3.3）：无法解析时给出提示；能解析则预览下一次触发。 */
function cronPreview(expr: string): { error?: string; next?: string } {
  if (!expr.trim()) return {};
  try {
    // cron-parser 5.x 的静态方法类型导出有误（解析为 0 参），运行时行为
    // 是 parse(expr, options)——此处收敛类型断言，错误经 try/catch 呈现。
    const parse = (CronExpressionParser as unknown as {
      parse: (expression: string, options?: { tz?: string }) => { next(): Date };
    }).parse;
    const interval = parse(expr, { tz: "Asia/Shanghai" });
    return { next: interval.next().toLocaleString("zh-CN", { hour12: false }) };
  } catch (error) {
    return { error: error instanceof Error ? error.message : "表达式无法解析" };
  }
}

/** 调度配置页（3.3）：按流水线新建/编辑调度。 */
export function ScheduleConfigPage() {
  const { message } = App.useApp();
  const queryClient = useQueryClient();
  const [form] = Form.useForm();
  const [saving, setSaving] = useState(false);

  const { data: schedules } = useQuery({
    queryKey: ["schedules"],
    queryFn: async () => {
      const result = await client.GET("/api/schedules", {});
      return unwrapData<ScheduleRow[]>(result);
    },
  });

  const { data: pipelines } = useQuery({
    queryKey: ["pipelines", 1],
    queryFn: async () => {
      const result = await client.GET("/api/pipelines", {
        params: { query: { page: 1, page_size: 100 } },
      });
      return unwrapData<{ items: components["schemas"]["PipelineSummaryResponse"][] }>(result);
    },
  });

  const cronValue = Form.useWatch("cron", form);

  const preview = cronPreview(cronValue ?? "");

  const onSave = async (values: {
    pipeline_id: number;
    cron?: string;
    enabled: boolean;
    catch_up: boolean;
    renewal_domains?: string[];
  }) => {
    setSaving(true);
    try {
      const result = await client.POST("/api/schedules", {
        body: {
          pipeline_id: values.pipeline_id,
          cron: values.cron || null,
          enabled: values.enabled,
          catch_up: values.catch_up,
          renewal_domains: values.renewal_domains?.length ? values.renewal_domains : null,
        },
      });
      if (result.error !== undefined) {
        const errorBody = result.error as { error?: { message?: string } };
        message.error(errorBody?.error?.message ?? "保存失败");
        return;
      }
      message.success("调度已保存（非法 cron 会被后端拒绝）");
      await queryClient.invalidateQueries({ queryKey: ["schedules"] });
    } finally {
      setSaving(false);
    }
  };

  return (
    <div>
      <Typography.Title level={4}>调度配置</Typography.Title>
      <Card title="新建 / 更新调度" style={{ marginBottom: 24 }}>
        <Form form={form} layout="vertical" onFinish={onSave}>
          <Space size={24} wrap align="start">
            <Form.Item
              name="pipeline_id"
              label="流水线"
              rules={[{ required: true, message: "请选择流水线" }]}
            >
              <Select
                style={{ width: 240 }}
                placeholder="选择流水线"
                options={(pipelines?.items ?? []).map((item) => ({
                  value: item.id,
                  label: item.name,
                }))}
              />
            </Form.Item>
            <Form.Item name="cron" label="cron 表达式（留空则仅参与续期扫描）">
              <Input style={{ width: 200 }} placeholder="0 3 * * *" />
            </Form.Item>
            <Form.Item name="enabled" label="启用" valuePropName="checked" initialValue={true}>
              <Switch />
            </Form.Item>
            <Form.Item name="catch_up" label="停机补跑" valuePropName="checked">
              <Switch />
            </Form.Item>
          </Space>
          {preview.error && (
            <Typography.Text type="danger">cron 错误：{preview.error}</Typography.Text>
          )}
          {preview.next && (
            <Typography.Text type="secondary" style={{ display: "block", marginBottom: 8 }}>
              下一次触发：{preview.next}
            </Typography.Text>
          )}
          <Form.Item name="renewal_domains" label="续期域名集合（命中任一域名即触发该流水线）">
            <Select mode="tags" open={false} placeholder="例如 example.com" tokenSeparators={[","]} />
          </Form.Item>
          <Button type="primary" htmlType="submit" loading={saving}>
            保存调度
          </Button>
        </Form>
      </Card>

      <Typography.Title level={5}>已有调度</Typography.Title>
      {schedules?.length ? (
        <Space direction="vertical" style={{ width: "100%" }}>
          {schedules.map((schedule) => (
            <Card key={schedule.pipeline_id} size="small">
              <Space wrap>
                <Typography.Text strong>流水线 #{schedule.pipeline_id}</Typography.Text>
                <Tag color={schedule.enabled ? "processing" : "default"}>
                  {schedule.enabled ? "启用" : "停用"}
                </Tag>
                {schedule.cron && <Tag>cron: {schedule.cron}</Tag>}
                {schedule.catch_up && <Tag color="warning">停机补跑</Tag>}
                {schedule.renewal_domains?.map((domain) => (
                  <Tag key={domain} color="cyan">
                    续期: {domain}
                  </Tag>
                ))}
                <Typography.Text type="secondary">
                  更新于 {formatTime(schedule.updated_at)}
                </Typography.Text>
              </Space>
            </Card>
          ))}
        </Space>
      ) : (
        <Typography.Text type="secondary">暂无调度配置</Typography.Text>
      )}
    </div>
  );
}
