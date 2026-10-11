import { useQuery, useQueryClient } from "@tanstack/react-query";
import { App, Button, Card, Form, Input, Modal, Select, Space, Switch, Table, Tag, Tooltip, Typography } from "antd";
import { CronExpressionParser } from "cron-parser";
import { useEffect, useState } from "react";
import { useNavigate } from "react-router";
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
    const parse = (CronExpressionParser as unknown as {
      parse: (expression: string, options?: { tz?: string }) => { next(): Date };
    }).parse;
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
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [editorOpen, setEditorOpen] = useState(false);
  const [editingSchedule, setEditingSchedule] = useState<ScheduleRow | null>(null);
  const [triggerPage, setTriggerPage] = useState(1);
  const [triggerPageSize, setTriggerPageSize] = useState(10);
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
    queryKey: ["trigger-logs", triggerPage, triggerPageSize, triggerPipelineFilter],
    queryFn: async () => {
      const result = await client.GET("/api/schedules/trigger-logs", {
        params: {
          query: {
            page: triggerPage,
            page_size: triggerPageSize,
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

  /** 流水线 id→name 映射：供悬浮名称与可点击跳转判定复用。 */
  const pipelinesMap = new Map<number, string>();
  for (const item of pipelines?.items ?? []) pipelinesMap.set(item.id, item.name);

  /** 已有调度卡片头部的流水线编号：存在时可点击跳转，已删除时纯文本提示。 */
  const renderPipelineBadge = (pipelineId: number) => {
    const name = pipelinesMap.get(pipelineId);
    if (name === undefined) {
      return (
        <Typography.Text strong title="流水线已删除">
          流水线 #{pipelineId}（已删除/未知流水线）
        </Typography.Text>
      );
    }
    return (
      <Tooltip title={name}>
        <Typography.Text
          strong
          onClick={() => navigate(`/pipelines/${pipelineId}`)}
          style={{ cursor: "pointer" }}
        >
          流水线 #{pipelineId}
        </Typography.Text>
      </Tooltip>
    );
  };

  /** 触发记录表“流水线”列：悬浮显示名称，缺失时降级占位。 */
  const renderTriggerPipeline = (pipelineId: number) => {
    const name = pipelinesMap.get(pipelineId);
    return (
      <Tooltip title={name ?? "已删除/未知流水线"}>
        <span>#{pipelineId}</span>
      </Tooltip>
    );
  };

  const openEditor = (schedule: ScheduleRow | null) => {
    setEditingSchedule(schedule);
    setEditorOpen(true);
  };

  const closeEditor = () => {
    setEditorOpen(false);
  };

  const handleSaved = async () => {
    setEditorOpen(false);
    await queryClient.invalidateQueries({ queryKey: ["schedules"] });
  };

  /** 删除调度：二次确认后 DELETE，成功刷新调度列表与触发记录。 */
  const confirmDelete = (schedule: ScheduleRow) => {
    Modal.confirm({
      title: "删除调度？",
      content: `流水线 #${schedule.pipeline_id} 的调度删除后无法恢复，关联的触发记录将一并清理。`,
      okButtonProps: { danger: true },
      okText: "删除",
      cancelText: "取消",
      onOk: async () => {
        const result = await client.DELETE(`/api/schedules/${schedule.pipeline_id}`, {
          params: { path: { pipeline_id: schedule.pipeline_id } },
        });
        if (result.error !== undefined) {
          const errorBody = result.error as { error?: { message?: string } };
          message.error(errorBody?.error?.message ?? "删除失败");
          return;
        }
        message.success("调度已删除");
        await queryClient.invalidateQueries({ queryKey: ["schedules"] });
        await queryClient.invalidateQueries({ queryKey: ["trigger-logs"] });
      },
    });
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
        extra={
          <Button type="primary" onClick={() => openEditor(null)}>
            新建调度
          </Button>
        }
      />

      <Typography.Title level={5}>已有调度</Typography.Title>
      {schedules?.length ? (
        <Space direction="vertical" style={{ width: "100%", marginBottom: 24 }}>
          {schedules.map((schedule) => (
            <Card key={schedule.pipeline_id} size="small">
              <Space wrap>
                {renderPipelineBadge(schedule.pipeline_id)}
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
                <Button size="small" onClick={() => openEditor(schedule)}>
                  编辑
                </Button>
                <Button size="small" danger onClick={() => confirmDelete(schedule)}>
                  删除
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
            pageSize: triggerPageSize,
            total: triggerLogs?.total ?? 0,
            onChange: (newPage, newPageSize) => {
              if (newPageSize !== triggerPageSize) {
                setTriggerPage(1);
                setTriggerPageSize(newPageSize);
              } else {
                setTriggerPage(newPage);
              }
            },
            showSizeChanger: true,
            pageSizeOptions: [10, 15, 20, 50, 100],
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
              render: (value: number) => renderTriggerPipeline(value),
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

      <ScheduleEditorModal
        open={editorOpen}
        editingSchedule={editingSchedule}
        pipelines={pipelines?.items ?? []}
        onClose={closeEditor}
        onSaved={handleSaved}
      />
    </div>
  );
}

function ScheduleEditorModal({
  open,
  editingSchedule,
  pipelines,
  onClose,
  onSaved,
}: {
  open: boolean;
  editingSchedule: ScheduleRow | null;
  pipelines: PipelineSummary[];
  onClose: () => void;
  onSaved: () => void;
}) {
  const { message } = App.useApp();
  const [form] = Form.useForm();
  const [saving, setSaving] = useState(false);

  const cronValue = Form.useWatch("cron", form);
  const enabledValue = Form.useWatch("enabled", form);
  const preview = cronPreview(cronValue ?? "");

  useEffect(() => {
    if (!open) return;
    if (editingSchedule) {
      form.setFieldsValue({
        pipeline_id: editingSchedule.pipeline_id,
        cron: editingSchedule.cron ?? undefined,
        enabled: editingSchedule.enabled,
        catch_up: editingSchedule.catch_up,
        renewal_domains: editingSchedule.renewal_domains ?? [],
      });
    } else {
      form.resetFields();
    }
  }, [open, editingSchedule, form]);

  const onSave = async (values: {
    pipeline_id: number;
    cron?: string;
    enabled: boolean;
    catch_up: boolean;
    renewal_domains?: string[];
  }) => {
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
      await onSaved();
    } finally {
      setSaving(false);
    }
  };

  return (
    <Modal
      open={open}
      title={editingSchedule === null ? "新建调度" : `编辑流水线 #${editingSchedule.pipeline_id} 的调度`}
      onCancel={onClose}
      onOk={() => form.submit()}
      okText="保存调度"
      cancelText="取消"
      confirmLoading={saving}
      destroyOnHidden
      width={640}
    >
      <Form form={form} layout="vertical" onFinish={onSave} initialValues={{ enabled: true, catch_up: false }}>
        <Form.Item
          name="pipeline_id"
          label="流水线"
          rules={[{ required: true, message: "请选择流水线" }]}
        >
          <Select
            placeholder="选择流水线"
            disabled={editingSchedule !== null}
            options={pipelines.map((item) => ({
              value: item.id,
              label: item.name,
            }))}
          />
        </Form.Item>
        <Form.Item name="cron" label="cron 表达式（留空则不按定时触发）">
          <Input placeholder="0 3 * * *" />
        </Form.Item>
        {preview.error && (
          <Typography.Text type="danger">cron 错误：{preview.error}</Typography.Text>
        )}
        {preview.next && (
          <Typography.Text type="secondary" style={{ display: "block", marginBottom: 8 }}>
            下一次触发（按 UTC 判定，与后端一致）：{preview.next}
          </Typography.Text>
        )}
        <Space size={24} wrap>
          <Form.Item name="enabled" label="启用" valuePropName="checked">
            <Switch />
          </Form.Item>
          <Form.Item name="catch_up" label="停机补跑" valuePropName="checked">
            <Switch />
          </Form.Item>
        </Space>
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
      </Form>
    </Modal>
  );
}
