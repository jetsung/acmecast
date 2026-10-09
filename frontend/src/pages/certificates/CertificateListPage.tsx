import { useQuery, useQueryClient } from "@tanstack/react-query";
import { App, Button, Input, Modal, Select, Space, Table, Tag, Typography } from "antd";
import type { TableColumnsType } from "antd";
import type { components } from "@/api/schema";
import { useState } from "react";
import { useNavigate } from "react-router";
import { client } from "@/api/client";
import { unwrapData } from "@/api/helpers";
import { formatTime } from "@/api/helpers";
import { PageHeader } from "@/components/PageHeader";
import { Panel } from "@/components/Panel";

type CertificateRow = components["schemas"]["CertificateResponse"];

/** 描述单元格：展示备注并提供内联编辑——点击"编辑"弹出 Modal，保存走 PATCH。 */
function DescriptionCell({
  row,
  onSaved,
}: {
  row: CertificateRow;
  onSaved: () => void;
}) {
  const { message } = App.useApp();
  const [open, setOpen] = useState(false);
  const [value, setValue] = useState(row.description ?? "");
  const [saving, setSaving] = useState(false);

  const save = async () => {
    setSaving(true);
    try {
      const result = await client.PATCH(`/api/certificates/${row.id}`, {
        params: { path: { id: row.id } },
        body: { description: value.trim() === "" ? null : value },
      });
      if (result.error !== undefined) {
        const detail = result.error as { error?: { message?: string } };
        message.error(detail.error?.message ?? "保存失败");
        return;
      }
      message.success("备注已保存");
      setOpen(false);
      onSaved();
    } finally {
      setSaving(false);
    }
  };

  return (
    <>
      <Button
        size="small"
        onClick={() => {
          setValue(row.description ?? "");
          setOpen(true);
        }}
        aria-label={`编辑证书 ${row.id} 的备注`}
      >
        编辑
      </Button>
      <Modal
        title={`编辑证书 ${row.id} 备注`}
        open={open}
        onCancel={() => setOpen(false)}
        footer={null}
        destroyOnHidden
      >
        <Space.Compact style={{ width: "100%" }}>
          <Input
            value={value}
            maxLength={100}
            placeholder="备注用途/归属"
            aria-label={`证书 ${row.id} 备注输入`}
            onChange={(event) => setValue(event.target.value)}
            onPressEnter={() => void save()}
          />
          <Button type="primary" loading={saving} onClick={() => void save()}>
            保存
          </Button>
        </Space.Compact>
      </Modal>
    </>
  );
}

/** 证书列表（2.1）：分页、域名搜索、到期排序、警示标记。 */
export function CertificateListPage() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const { message, modal } = App.useApp();
  const [domain, setDomain] = useState("");
  const [sort, setSort] = useState<"ascending" | "descending">("descending");
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(15);
  const [revokingId, setRevokingId] = useState<number | null>(null);
  const [archivingId, setArchivingId] = useState<number | null>(null);
  const [deletingId, setDeletingId] = useState<number | null>(null);

  const { data, isFetching } = useQuery({
    queryKey: ["certificates", domain, sort, page, pageSize],
    queryFn: async () => {
      const result = await client.GET("/api/certificates", {
        params: {
          query: {
            domain: domain || undefined,
            sort,
            page,
            page_size: pageSize,
          },
        },
      });
      return unwrapData(result);
    },
  });

  const confirmRevoke = (row: CertificateRow) => {
    modal.confirm({
      title: "确认吊销证书？",
      content: `将向 CA 发起吊销：${row.domains.join(", ")}。吊销后证书立即失效，且不再参与自动续期。`,
      okText: "吊销",
      okButtonProps: { danger: true },
      cancelText: "取消",
      onOk: async () => {
        setRevokingId(row.id);
        try {
          const result = await client.POST(`/api/certificates/${row.id}/revoke`, {
            params: { path: { id: row.id } },
            body: {},
          });
          if (result.error !== undefined && typeof result.error === "object" && result.error !== null && "error" in result.error) {
            const apiErr = result.error as { error?: { message?: string } };
            message.error(apiErr.error?.message ?? "吊销失败");
            return;
          }
          if (result.error !== undefined) {
            message.error("吊销失败");
            return;
          }
          message.success("证书已吊销，本地状态已更新");
          await queryClient.invalidateQueries({ queryKey: ["certificates"] });
        } finally {
          setRevokingId(null);
        }
      },
    });
  };

  const confirmArchive = (row: CertificateRow) => {
    modal.confirm({
      title: "确认归档证书？",
      content: `将把证书文件移至归档目录：${row.domains.join(", ")}。归档后仍可下载查看。`,
      okText: "归档",
      cancelText: "取消",
      okButtonProps: { color: "orange", variant: "outlined" as const },
      onOk: async () => {
        setArchivingId(row.id);
        try {
          const result = await client.POST(`/api/certificates/${row.id}/archive`, {
            params: { path: { id: row.id } },
          });
          if (result.error !== undefined) {
            let msg = "归档失败";
            if (typeof result.error === "object" && result.error !== null && "error" in result.error) {
              const apiErr = result.error as { error?: { message?: string } };
              if (apiErr.error?.message) msg = apiErr.error.message;
            }
            message.error(msg);
            return Promise.reject(new Error(msg));
          }
          message.success("证书已归档");
          await queryClient.invalidateQueries({ queryKey: ["certificates"] });
          await queryClient.refetchQueries({ queryKey: ["certificates"] });
        } finally {
          setArchivingId(null);
        }
      },
    });
  };

  const confirmDelete = (row: CertificateRow) => {
    modal.confirm({
      title: "确认删除证书？",
      content: `将彻底删除证书（含文件与记录）：${row.domains.join(", ")}。仅归档或吊销态可删除，删除后不可恢复。`,
      okText: "删除",
      okButtonProps: { danger: true },
      cancelText: "取消",
      onOk: async () => {
        setDeletingId(row.id);
        try {
          const result = await client.DELETE(`/api/certificates/${row.id}`, {
            params: { path: { id: row.id } },
          });
          if (result.error !== undefined) {
            let msg = "删除失败";
            if (typeof result.error === "object" && result.error !== null && "error" in result.error) {
              const apiErr = result.error as { error?: { message?: string } };
              if (apiErr.error?.message) msg = apiErr.error.message;
            }
            message.error(msg);
            return Promise.reject(new Error(msg));
          }
          message.success("证书已删除");
          await queryClient.invalidateQueries({ queryKey: ["certificates"] });
          await queryClient.refetchQueries({ queryKey: ["certificates"] });
        } finally {
          setDeletingId(null);
        }
      },
    });
  };

  const columns: TableColumnsType<CertificateRow> = [
    {
      title: "ID",
      dataIndex: "id",
      width: 80,
      render: (id: number) => String(id),
    },
    {
      title: "域名",
      dataIndex: "domains",
      render: (domains: string[]) => domains.join(", "),
    },
    {
      title: "到期时间",
      dataIndex: "not_after",
      width: 260,
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
      title: "算法",
      dataIndex: "key_algorithm",
      width: 110,
      render: (algorithm: string | null | undefined) =>
        algorithm ? (
          <Tag color="geekblue">{algorithm}</Tag>
        ) : (
          <Typography.Text type="secondary">—</Typography.Text>
        ),
    },
    {
      title: "描述",
      dataIndex: "description",
      ellipsis: true,
      render: (value: string | null | undefined) =>
        value && value.trim() !== "" ? (
          <Typography.Text ellipsis style={{ maxWidth: 280 }} title={value}>
            {value}
          </Typography.Text>
        ) : (
          <Typography.Text type="secondary">—</Typography.Text>
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
      width: 340,
      render: (_: unknown, row: CertificateRow) => {
        const isRevoked = row.revoked_at !== null && String(row.revoked_at).trim() !== "";
        const isArchived = String((row as { archived_at?: string | null }).archived_at ?? "").trim() !== "";
        const canDelete = isRevoked || isArchived;
        return (
          // stopPropagation：避免点击操作触发行的跳转导航。
          <Space onClick={(event) => event.stopPropagation()}>
            <DescriptionCell
              row={row}
              onSaved={() => void queryClient.invalidateQueries({ queryKey: ["certificates"] })}
            />
            <Button
              size="small"
              danger
              disabled={isRevoked}
              loading={revokingId === row.id}
              onClick={() => confirmRevoke(row)}
              aria-label={`吊销证书 ${row.id}`}
            >
              吊销
            </Button>
            <Button
              size="small"
              color="orange"
              variant="outlined"
              disabled={isRevoked || isArchived}
              loading={archivingId === row.id}
              onClick={() => confirmArchive(row)}
              aria-label={`归档证书 ${row.id}`}
            >
              归档
            </Button>
            <Button
              size="small"
              danger
              disabled={!canDelete}
              loading={deletingId === row.id}
              onClick={() => confirmDelete(row)}
              aria-label={`删除证书 ${row.id}`}
            >
              删除
            </Button>
          </Space>
        );
      },
    },
  ];

  return (
    <div>
      <PageHeader
        title="证书"
        description="已签发证书的库存与有效期"
        extra={
          <>
            <Input.Search
              placeholder="按域名搜索"
              allowClear
              style={{ width: 240 }}
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
          </>
        }
      />
      <Panel>
        <Table<CertificateRow>
          rowKey="id"
          loading={isFetching}
          columns={columns}
          dataSource={data?.items ?? []}
          rowClassName={(row) => (row.revoked_at ? "table-row-revoked" : "")}
          onRow={(row) => ({ onClick: () => navigate(`/certificates/${row.id}`), style: { cursor: "pointer" } })}
          pagination={{
            current: page,
            pageSize,
            total: data?.total ?? 0,
            onChange: (newPage, newPageSize) => {
              if (newPageSize !== pageSize) {
                setPage(1);
                setPageSize(newPageSize);
              } else {
                setPage(newPage);
              }
            },
            showSizeChanger: true,
            pageSizeOptions: [15, 20, 50, 100],
            showTotal: (total) => `共 ${total} 条`,
          }}
        />
      </Panel>
    </div>
  );
}
