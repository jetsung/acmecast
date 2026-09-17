import { useQuery } from "@tanstack/react-query";
import { Input, Select, Space, Table, Tag, Typography } from "antd";
import type { TableColumnsType } from "antd";
import type { components } from "@/api/schema";
import { useState } from "react";
import { useNavigate } from "react-router";
import { client } from "@/api/client";
import { unwrapData } from "@/api/helpers";
import { formatTime } from "@/api/helpers";

type CertificateRow = components["schemas"]["CertificateResponse"];

/** 证书列表（2.1）：分页、域名搜索、到期排序、警示标记。 */
export function CertificateListPage() {
  const navigate = useNavigate();
  const [domain, setDomain] = useState("");
  const [sort, setSort] = useState<"ascending" | "descending">("ascending");
  const [page, setPage] = useState(1);

  const { data, isFetching } = useQuery({
    queryKey: ["certificates", domain, sort, page],
    queryFn: async () => {
      const result = await client.GET("/api/certificates", {
        params: {
          query: {
            domain: domain || undefined,
            sort,
            page,
            page_size: 20,
          },
        },
      });
      return unwrapData(result);
    },
  });

  const columns: TableColumnsType<CertificateRow> = [
    {
      title: "域名",
      dataIndex: "domains",
      render: (domains: string[]) => domains.join(", "),
    },
    {
      title: "到期时间",
      dataIndex: "not_after",
      width: 200,
      sorter: true,
      render: (value: string, row) => {
        const days = Math.floor(
          (new Date(value).getTime() - Date.now()) / (24 * 3600 * 1000),
        );
        const color = row.revoked_at
          ? "default"
          : days <= 30
            ? "red"
            : days <= 60
              ? "orange"
              : "green";
        return (
          <Space>
            <Tag color={color}>{days <= 0 ? "已过期" : `${days} 天`}</Tag>
            {formatTime(value)}
          </Space>
        );
      },
    },
    {
      title: "状态",
      dataIndex: "revoked_at",
      width: 100,
      render: (revoked: string | null) =>
        revoked ? <Tag color="default">已吊销</Tag> : <Tag color="processing">有效</Tag>,
    },
    {
      title: "更新时间",
      dataIndex: "updated_at",
      width: 200,
      render: (value: string) => formatTime(value),
    },
  ];

  return (
    <div>
      <Typography.Title level={4}>证书</Typography.Title>
      <Space style={{ marginBottom: 16 }}>
        <Input.Search
          placeholder="按域名搜索"
          allowClear
          style={{ width: 280 }}
          onSearch={(value) => {
            setDomain(value);
            setPage(1);
          }}
        />
        <Select
          value={sort}
          style={{ width: 160 }}
          onChange={(value) => setSort(value)}
          options={[
            { value: "ascending", label: "按到期时间升序" },
            { value: "descending", label: "按到期时间降序" },
          ]}
        />
      </Space>
      <Table<CertificateRow>
        rowKey="id"
        loading={isFetching}
        columns={columns}
        dataSource={data?.items ?? []}
        rowClassName={(row) => (row.revoked_at ? "table-row-revoked" : "")}
        onRow={(row) => ({ onClick: () => navigate(`/certificates/${row.id}`), style: { cursor: "pointer" } })}
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
