import { useQuery } from "@tanstack/react-query";
import { Col, Row, Table, Tag } from "antd";
import type { TableColumnsType } from "antd";
import {
  SafetyCertificateOutlined,
  WarningOutlined,
  CloseCircleOutlined,
} from "@ant-design/icons";
import type { components } from "@/api/schema";
import { client } from "@/api/client";
import { formatTime, unwrapData } from "@/api/helpers";
import { PageHeader } from "@/components/PageHeader";
import { StatCard } from "@/components/StatCard";
import { Panel } from "@/components/Panel";

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
      <PageHeader
        title="仪表盘"
        description="证书有效期与流水线运行的总体概况"
      />
      <Row gutter={16} style={{ marginBottom: 24 }}>
        <Col span={8}>
          <StatCard
            title="有效证书"
            value={
              <>
                {active.length}
                <span style={{ fontSize: 16, fontWeight: 400, color: "#8a93a6" }}>
                  {" "}
                  / {certs?.total ?? 0}
                </span>
              </>
            }
            icon={<SafetyCertificateOutlined />}
            tone={{ bg: "#e8f7ef", color: "#16a05d" }}
          />
        </Col>
        <Col span={8}>
          <StatCard
            title="30 天内到期"
            value={active.filter((cert) => new Date(cert.not_after).getTime() <= soonThreshold).length}
            icon={<WarningOutlined />}
            tone={{ bg: "#fdf1e3", color: "#e8830c" }}
            valueColor="#e8830c"
          />
        </Col>
        <Col span={8}>
          <StatCard
            title="最近失败运行"
            value={(histories?.items ?? []).filter((item) => item.status === "failed").length}
            icon={<CloseCircleOutlined />}
            tone={{ bg: "#fdecec", color: "#e0483e" }}
            valueColor="#e0483e"
          />
        </Col>
      </Row>

      <Panel title="最早到期的证书" style={{ marginBottom: 24 }}>
        <Table<CertificateRow>
          rowKey="id"
          columns={columns}
          dataSource={expiringSoon}
          pagination={false}
          size="small"
        />
      </Panel>

      <Panel title="最近运行">
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
      </Panel>
    </div>
  );
}
