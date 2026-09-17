import { useQuery } from "@tanstack/react-query";
import { Card, Col, Row, Statistic, Table, Tag, Typography } from "antd";
import type { TableColumnsType } from "antd";
import type { components } from "@/api/schema";
import { client } from "@/api/client";
import { formatTime, unwrapData } from "@/api/helpers";

type CertificateRow = components["schemas"]["CertificateResponse"];

type HistoryRow = components["schemas"]["HistoryResponse"];

const STATUS_TAG: Record<string, { label: string; color: string }> = {
  running: { label: "运行中", color: "processing" },
  success: { label: "成功", color: "success" },
  failed: { label: "失败", color: "error" },
};

/** 仪表盘（3.4）：数据全部来自现有端点组合，不新增后端端点（design D8）。 */
export function DashboardPage() {
  // 最早到期的证书（升序取前 5 条，排除已吊销在后端 list_due 之外——这里直接列表排序取前 5）。
  const { data: certs } = useQuery({
    queryKey: ["certificates", "dashboard"],
    queryFn: async () => {
      const result = await client.GET("/api/certificates", {
        params: { query: { sort: "ascending", page: 1, page_size: 100 } },
      });
      return unwrapData<{ items: CertificateRow[]; total: number }>(result);
    },
  });

  const { data: histories } = useQuery({
    queryKey: ["histories", "dashboard"],
    queryFn: async () => {
      const result = await client.GET("/api/histories", {
        params: { query: { page: 1, page_size: 8 } },
      });
      return unwrapData<{ items: HistoryRow[]; total: number }>(result);
    },
  });

  const active = (certs?.items ?? []).filter((cert) => !cert.revoked_at);
  const soonThreshold = Date.now() + 30 * 24 * 3600 * 1000;
  const expiringSoon = active
    .filter((cert) => new Date(cert.not_after).getTime() <= soonThreshold)
    .slice(0, 5);

  const columns: TableColumnsType<CertificateRow> = [
    {
      title: "域名",
      dataIndex: "domains",
      render: (domains: string[]) => domains.join(", "),
    },
    {
      title: "到期",
      dataIndex: "not_after",
      width: 220,
      render: (value: string) => {
        const days = Math.floor((new Date(value).getTime() - Date.now()) / (24 * 3600 * 1000));
        return (
          <Tag color={days <= 30 ? "red" : days <= 60 ? "orange" : "green"}>
            {days <= 0 ? "已过期" : `${days} 天`}
          </Tag>
        );
      },
    },
  ];

  return (
    <div>
      <Typography.Title level={4}>仪表盘</Typography.Title>
      <Row gutter={16} style={{ marginBottom: 24 }}>
        <Col span={8}>
          <Card>
            <Statistic title="有效证书" value={active.length} suffix={`/ ${certs?.total ?? 0}`} />
          </Card>
        </Col>
        <Col span={8}>
          <Card>
            <Statistic
              title="30 天内到期"
              value={active.filter((cert) => new Date(cert.not_after).getTime() <= soonThreshold).length}
              valueStyle={{ color: "#cf1322" }}
            />
          </Card>
        </Col>
        <Col span={8}>
          <Card>
            <Statistic
              title="最近失败运行"
              value={(histories?.items ?? []).filter((item) => item.status === "failed").length}
              valueStyle={{ color: "#cf1322" }}
            />
          </Card>
        </Col>
      </Row>

      <Typography.Title level={5}>最早到期的证书</Typography.Title>
      <Table<CertificateRow>
        rowKey="id"
        columns={columns}
        dataSource={expiringSoon}
        pagination={false}
        size="small"
        style={{ marginBottom: 24 }}
      />

      <Typography.Title level={5}>最近运行</Typography.Title>
      <Table<HistoryRow>
        rowKey="id"
        columns={[
          {
            title: "流水线",
            dataIndex: "pipeline_id",
            width: 100,
            render: (value: number) => `#${value}`,
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
            title: "触发来源",
            dataIndex: "trigger_source",
            width: 100,
            render: (source: string) =>
              ({ manual: "手动", cron: "定时", renewal: "续期" })[source] ?? source,
          },
          { title: "开始时间", dataIndex: "started_at", render: (value: string) => formatTime(value) },
        ]}
        dataSource={histories?.items ?? []}
        pagination={false}
        size="small"
      />
    </div>
  );
}
