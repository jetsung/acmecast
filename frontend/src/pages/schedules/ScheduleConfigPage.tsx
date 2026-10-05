import { useQuery, useQueryClient } from "@tanstack/react-query";
import { App, Button, Card, Form, Input, Select, Space, Switch, Table, Tag, Typography } from "antd";
import { CronExpressionParser } from "cron-parser";
import { useState } from "react";
import type { components } from "@/api/schema";
import { client } from "@/api/client";
import { formatTime, unwrapData } from "@/api/helpers";
import { PageHeader } from "@/components/PageHeader";

type ScheduleRow = components["schemas"]["ScheduleResponse"];
type TriggerLogRow = components["schemas"]["TriggerLogResponse"];
type PipelineSummary = components["schemas"]["PipelineSummaryResponse"];

const TRIGGER_SOURCE_LABEL: Record<string, string> = {
  cron: "cron",
  renewal: "续期",
};

/** cron 前置校验（3.3）：无法解析时给出提示；能解析则预览下一次触发。 */
function cronPreview(expr: string): { error?: string; next?: string } {
  if (!expr.trim()) return {};
  try {
    // cron-parser 5.x 的静态方法类型导出有误（解析为 0 参），运行时行为
    // 是 parse(expr, options)——此处收敛类型断言，错误经 try/catch 呈现。
    const parse = (CronExpressionParser as unknown as {
      parse: (expression: string, options?: { tz?: string }) => { next(): Date };
    }).parse;
    // 后端触发判定按 UTC，预览必须同口径（决策 1）：以 UTC 计算下一次
    // 触发点，展示本地时间的同时明确标出 UTC 时刻。
    const next = parse(expr, { tz: "UTC" }).next();
    const local = next.toLocaleString("zh-CN", { hour12: false });
    const utc = next.toISOString().replace("T", " ").slice(0, 16);
    return { next: `${local}（UTC ${utc}）` };
  } catch (error) {
    return { error: error instanceof Error ? error.message : "表达式无法解析" };
  }
}

/** 调度配置页（3.3）：按流水线新建/编辑调度，并查看运行态与触发记录。 */
export function ScheduleConfigPage() {
  const { message } = App.useApp();
  const queryClient = useQueryClient();
  const [form] = Form.useForm();
  const [saving, setSaving] = useState(false);
  /** 正在编辑的流水线 id；null 表示新建模式。 */
  const [editingPipelineId, setEditingPipelineId] = useState<number | null>(null);
  const [triggerPage, setTriggerPage] = useState(1);
  const [triggerPipelineFilter, setTriggerPipelineFilter] = useState<number | null>(null);

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
      return unwrapData<{ items: PipelineSummary[] }>(result);
    },
  });

  const { data: triggerLogs } = useQuery({
    queryKey: ["trigger-logs", triggerPage, triggerPipelineFilter],
    queryFn: async () => {
      const result = await client.GET("/api/schedules/trigger-logs", {
        params: {
          query: {
            page: triggerPage,
            page_size: 10,
            ...(triggerPipelineFilter !== null ? { pipeline_id: triggerPipelineFilter } : {}),
          },
        },
      });
      return unwrapData<{
        items: TriggerLogRow[];
        total: number;
        page: number;
        page_size: number;
      }>(result);
    },
  });

  const cronValue = Form.useWatch("cron", form);
  const enabledValue = Form.useWatch("enabled", form);

  const preview = cronPreview(cronValue ?? "");

  const onSave = async (values: {
    pipeline_id: number;
    cron?: string;
    enabled: boolean;
    catch_up: boolean;
    renewal_domains?: string[];
  }) => {
    // 与后端保存校验同口径的前置拦截：启用的调度两个触发字段都留空
    // 只会得到一条永不自动触发的死配置，直接不发请求。
    if (values.enabled && !values.cron?.trim() && !values.renewal_domains?.length) {
      message.error("启用的调度必须至少配置 cron 或续期域名集合之一");
      return;
    }
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
      message.success("调度已保存");
      exitEditMode();
      await queryClient.invalidateQueries({ queryKey: ["schedules"] });
      await queryClient.invalidateQueries({ queryKey: ["trigger-logs"] });
    } finally {
      setSaving(false);
    }
  };

  /** 把该流水线的调度现值回填进表单，进入编辑模式。 */
  const startEdit = (schedule: ScheduleRow) => {
    setEditingPipelineId(schedule.pipeline_id);
    form.setFieldsValue({
      pipeline_id: schedule.pipeline_id,
      cron: schedule.cron ?? undefined,
      enabled: schedule.enabled,
      catch_up: schedule.catch_up,
      renewal_domains: schedule.renewal_domains ?? [],
    });
  };

  const exitEditMode = () => {
    setEditingPipelineId(null);
    form.resetFields();
  };

  /** 快捷启停：以该行当前字段整体保存、仅翻转 enabled（保存即 upsert 覆盖）。 */
  const toggleEnabled = async (schedule: ScheduleRow, enabled: boolean) => {
    const result = await client.POST("/api/schedules", {
      body: {
        pipeline_id: schedule.pipeline_id,
        cron: schedule.cron,
        enabled,
        catch_up: schedule.catch_up,
        renewal_domains: schedule.renewal_domains ?? null,
      },
    });
    if (result.error !== undefined) {
      const errorBody = result.error as { error?: { message?: string } };
      message.error(errorBody?.error?.message ?? "操作失败");
      // 开关受控行数据渲染，刷新失败即回滚到原状态。
      await queryClient.invalidateQueries({ queryKey: ["schedules"] });
      return;
    }
    message.success(enabled ? "调度已启用" : "调度已停用");
    await queryClient.invalidateQueries({ queryKey: ["schedules"] });
  };

  return (
    <div>
      <PageHeader
        title="调度配置"
        description="按 cron 或证书到期扫描自动触发流水线"
      />
      <Card
        title={editingPipelineId === null ? "新建调度" : `编辑流水线 #${editingPipelineId} 的调度`}
        style={{ marginBottom: 24 }}
        extra={
          editingPipelineId === null ? undefined : (
            <Button onClick={exitEditMode}>取消编辑</Button>
          )
        }
      >
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
                disabled={editingPipelineId !== null}
                options={(pipelines?.items ?? []).map((item) => ({
                  value: item.id,
                  label: item.name,
                }))}
              />
            </Form.Item>
            <Form.Item name="cron" label="cron 表达式（留空则不按定时触发）">
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
              下一次触发（按 UTC 判定，与后端一致）：{preview.next}
            </Typography.Text>
          )}
          <Form.Item
            name="renewal_domains"
            label="续期域名集合（非必填；留空则不参与证书到期自动续期）"
            extra="证书剩余有效期不足 30 天进入续期窗口后，域名命中任一条即触发本流水线；扫描每小时一次。"
          >
            <Select mode="tags" open={false} placeholder="例如 example.com" tokenSeparators={[","]} />
          </Form.Item>
          {enabledValue && !preview.next && (
            <Typography.Text type="warning" style={{ display: "block", marginBottom: 8 }}>
              未填写 cron：该调度仅按证书到期扫描触发；若续期域名集合也留空则不会被保存。
            </Typography.Text>
          )}
          <Button type="primary" htmlType="submit" loading={saving}>
            保存调度
          </Button>
        </Form>
      </Card>

      <Typography.Title level={5}>已有调度</Typography.Title>
      {schedules?.length ? (
        <Space direction="vertical" style={{ width: "100%", marginBottom: 24 }}>
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
                  下次触发：{formatTime(schedule.next_trigger_at)}
                </Typography.Text>
                <Typography.Text type="secondary">
                  上次触发：{formatTime(schedule.last_triggered_at)}
                </Typography.Text>
                <Switch
                  size="small"
                  checked={schedule.enabled}
                  checkedChildren="启用"
                  unCheckedChildren="停用"
                  aria-label={`流水线 ${schedule.pipeline_id} 的调度启停开关`}
                  onChange={(checked) => void toggleEnabled(schedule, checked)}
                />
                <Button size="small" onClick={() => startEdit(schedule)}>
                  编辑
                </Button>
                <Typography.Text type="secondary">
                  更新于 {formatTime(schedule.updated_at)}
                </Typography.Text>
              </Space>
            </Card>
          ))}
        </Space>
      ) : (
        <Typography.Text type="secondary" style={{ display: "block", marginBottom: 24 }}>
          暂无调度配置
        </Typography.Text>
      )}

      <Typography.Title level={5}>触发记录</Typography.Title>
      <Card size="small" title="调度触发的审计记录（每次真实启动一条）">
        <Space style={{ marginBottom: 12 }} wrap>
          <Select
            style={{ width: 240 }}
            allowClear
            placeholder="按流水线过滤"
            value={triggerPipelineFilter ?? undefined}
            onChange={(value) => {
              setTriggerPipelineFilter(value ?? null);
              setTriggerPage(1);
            }}
            options={(schedules ?? []).map((schedule) => ({
              value: schedule.pipeline_id,
              label: `流水线 #${schedule.pipeline_id}`,
            }))}
          />
        </Space>
        <Table<TriggerLogRow>
          size="small"
          rowKey="id"
          loading={triggerLogs === undefined}
          dataSource={triggerLogs?.items ?? []}
          locale={{ emptyText: "暂无触发记录" }}
          pagination={{
            current: triggerLogs?.page ?? triggerPage,
            pageSize: triggerLogs?.page_size ?? 10,
            total: triggerLogs?.total ?? 0,
            onChange: (page) => setTriggerPage(page),
            showSizeChanger: false,
          }}
          columns={[
            {
              title: "触发时间",
              dataIndex: "triggered_at",
              render: (value: string) => formatTime(value),
            },
            {
              title: "流水线",
              dataIndex: "pipeline_id",
              render: (value: number) => `#${value}`,
            },
            {
              title: "来源",
              dataIndex: "source",
              render: (value: string) => (
                <Tag color={value === "cron" ? "blue" : "cyan"}>
                  {TRIGGER_SOURCE_LABEL[value] ?? value}
                </Tag>
              ),
            },
            { title: "说明", dataIndex: "detail", ellipsis: true },
          ]}
        />
      </Card>
    </div>
  );
}
