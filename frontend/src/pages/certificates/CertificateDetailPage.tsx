import { useQuery, useQueryClient } from "@tanstack/react-query";
import { App, Button, Card, Descriptions, Space, Typography } from "antd";
import type { components } from "@/api/schema";
import { useParams } from "react-router";
import { useState } from "react";
import { client } from "@/api/client";
import { downloadFile, formatTime, unwrapData } from "@/api/helpers";
import { ApiError } from "@/api/client";
import { useAuthStore } from "@/auth/store";
import { PageHeader } from "@/components/PageHeader";

type CertificateDetail = components["schemas"]["CertificateResponse"];

const FORMATS = [
  { value: "pem", label: "PEM (证书链)" },
  { value: "der", label: "DER (二进制)" },
  { value: "pfx", label: "PFX (Windows)" },
  { value: "jks", label: "JKS (Java)" },
  { value: "p7b", label: "P7B" },
];

/** 证书详情（2.2）：信息展示、多格式下载、吊销（二次确认）。 */
export function CertificateDetailPage() {
  const { id } = useParams();
  const certId = Number(id);
  const { message, modal } = App.useApp();
  const queryClient = useQueryClient();
  const token = useAuthStore((state) => state.token);
  const [downloading, setDownloading] = useState<string | null>(null);
  const [archiving, setArchiving] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const { data: cert, error } = useQuery({
    queryKey: ["certificate", certId],
    queryFn: async () => {
      const result = await client.GET(`/api/certificates/${certId}`, {
        params: { path: { id: certId } },
      });
      if (result.error !== undefined) {
        throw new ApiError(result.response.status, undefined);
      }
      return await unwrapData<CertificateDetail>(result);
    },
    enabled: Number.isFinite(certId),
  });

  if (error) {
    return <Typography.Text type="danger">证书不存在或加载失败</Typography.Text>;
  }
  if (!cert) {
    return <Typography.Text type="secondary">加载中…</Typography.Text>;
  }

  const doDownload = async (format: string) => {
    setDownloading(format);
    try {
      await downloadFile(
        `/api/certificates/${certId}/download?format=${format}`,
        token,
      );
    } catch (err) {
      message.error(err instanceof Error ? err.message : "下载失败");
    } finally {
      setDownloading(null);
    }
  };

  const isRevoked = typeof cert.revoked_at === "string" && cert.revoked_at.trim() !== "";
  const isArchived = typeof cert.archived_at === "string" && cert.archived_at.trim() !== "";

  const confirmArchive = () => {
    modal.confirm({
      title: "确认归档证书？",
      content: `将把证书文件移至归档目录：${cert.domains.join(", ")}。归档后仍可下载查看。`,
      okText: "归档",
      cancelText: "取消",
      okButtonProps: { color: "orange", variant: "outlined" as const },
      onOk: async () => {
        setArchiving(true);
        try {
          const result = await client.POST(`/api/certificates/${certId}/archive`, {
            params: { path: { id: certId } },
          });
          if (result.error !== undefined) {
            let msg = "归档失败";
            if (
              result.error !== null &&
              typeof result.error === "object" &&
              "error" in result.error
            ) {
              const inner = result.error.error;
              if (
                inner !== null &&
                typeof inner === "object" &&
                "message" in inner
              ) {
                const maybeMsg = inner.message;
                if (typeof maybeMsg === "string" && maybeMsg.trim() !== "") {
                  msg = maybeMsg;
                }
              }
            }
            message.error(msg);
            return;
          }
          message.success("证书已归档");
          await queryClient.invalidateQueries({ queryKey: ["certificate", certId] });
          await queryClient.invalidateQueries({ queryKey: ["certificates"] });
        } finally {
          setArchiving(false);
        }
      },
    });
  };

  const confirmDelete = () => {
    modal.confirm({
      title: "确认删除证书？",
      content: `将彻底删除证书（含文件与记录）：${cert.domains.join(", ")}。仅归档或吊销态可删除，删除后不可恢复。`,
      okText: "删除",
      okButtonProps: { danger: true },
      cancelText: "取消",
      onOk: async () => {
        setDeleting(true);
        try {
          const result = await client.DELETE(`/api/certificates/${certId}`, {
            params: { path: { id: certId } },
          });
          if (result.error !== undefined) {
            let msg = "删除失败";
            if (result.error !== null && typeof result.error === "object" && "error" in result.error) {
              const inner = (result.error as { error?: { message?: string } }).error;
              if (inner?.message && inner.message.trim() !== "") msg = inner.message;
            }
            message.error(msg);
            return;
          }
          message.success("证书已删除");
          await queryClient.invalidateQueries({ queryKey: ["certificates"] });
          await queryClient.invalidateQueries({ queryKey: ["certificate", certId] });
        } finally {
          setDeleting(false);
        }
      },
    });
  };

  const confirmRevoke = () => {
    modal.confirm({
      title: "确认吊销证书？",
      content: `将向 CA 发起吊销：${cert.domains.join(", ")}。吊销后证书立即失效，且不再参与自动续期。`,
      okText: "吊销",
      okButtonProps: { danger: true },
      cancelText: "取消",
      onOk: async () => {
        const result = await client.POST(`/api/certificates/${certId}/revoke`, {
          params: { path: { id: certId } },
          body: {},
        });
        if (result.error !== undefined) {
          let msg = "吊销失败";
          if (
            result.error !== null &&
            typeof result.error === "object" &&
            "error" in result.error
          ) {
            const inner = result.error.error;
            if (
              inner !== null &&
              typeof inner === "object" &&
              "message" in inner
            ) {
              const maybeMsg = inner.message;
              if (typeof maybeMsg === "string" && maybeMsg.trim() !== "") {
                msg = maybeMsg;
              }
            }
          }
          message.error(msg);
          return;
        }
        message.success("证书已吊销，本地状态已更新");
        await queryClient.invalidateQueries({ queryKey: ["certificate", certId] });
        await queryClient.invalidateQueries({ queryKey: ["certificates"] });
      },
    });
  };

  return (
    <div>
      <PageHeader
        title={`证书 #${cert.id}`}
        subtitle={
          cert.revoked_at && (
            <Typography.Text type="secondary" style={{ fontSize: 14 }}>
              已于 {formatTime(cert.revoked_at)} 吊销
            </Typography.Text>
          )
        }
        description={cert.domains.join(", ")}
        extra={
          <Space wrap>
            {FORMATS.map((format) => (
              <Button
                key={format.value}
                disabled={cert.revoked_at !== null}
                loading={downloading === format.value}
                onClick={() => doDownload(format.value)}
              >
                {format.label}
              </Button>
            ))}
            <Button
              color="orange"
              variant="outlined"
              disabled={isRevoked || isArchived}
              loading={archiving}
              onClick={confirmArchive}
            >
              归档
            </Button>
            <Button
              danger
              disabled={!isRevoked && !isArchived}
              loading={deleting}
              onClick={confirmDelete}
            >
              删除
            </Button>
            <Button
              danger
              disabled={cert.revoked_at !== null || cert.acme_account_access_id === null}
              onClick={confirmRevoke}
            >
              吊销
            </Button>
          </Space>
        }
      />

      <Card>
        <Descriptions bordered column={1} size="small">
          <Descriptions.Item label="域名">{cert.domains.join(", ")}</Descriptions.Item>
          <Descriptions.Item label="签发者">{cert.issuer ?? "-"}</Descriptions.Item>
          <Descriptions.Item label="生效时间">{formatTime(cert.not_before)}</Descriptions.Item>
          <Descriptions.Item label="到期时间">{formatTime(cert.not_after)}</Descriptions.Item>
          <Descriptions.Item label="指纹">
            <Typography.Text code copyable>{cert.fingerprint}</Typography.Text>
          </Descriptions.Item>
          <Descriptions.Item label="签发账号凭据">
            {cert.acme_account_access_id ? `#${cert.acme_account_access_id}` : "未绑定（手动上传）"}
          </Descriptions.Item>
          <Descriptions.Item label="私钥算法">
            {cert.key_algorithm?.trim() ? cert.key_algorithm : "—"}
          </Descriptions.Item>
          <Descriptions.Item label="描述">
            {(() => {
              const text = cert.description?.trim();
              if (!text) return "—";
              return (
                <Typography.Text ellipsis={{ tooltip: text }} style={{ maxWidth: 480 }}>
                  {text}
                </Typography.Text>
              );
            })()}
          </Descriptions.Item>
          <Descriptions.Item label="证书状态">
            {(() => {
              const revoked = typeof cert.revoked_at === "string" && cert.revoked_at.trim() !== "";
              const archived = typeof cert.archived_at === "string" && (cert as unknown as { archived_at?: string | null }).archived_at !== null && String((cert as unknown as { archived_at?: string | null }).archived_at).trim() !== "";
              if (revoked) return <span style={{ color: "#999" }}>已吊销</span>;
              if (archived) return <span style={{ color: "#fa8c16" }}>已归档</span>;
              const days = Math.floor((new Date(cert.not_after).getTime() - Date.now()) / (24 * 3600 * 1000));
              if (days <= 0) return <span style={{ color: "#ff4d4f" }}>已过期</span>;
              if (days <= 30) return <span style={{ color: "#ff4d4f" }}>将过期（{days} 天）</span>;
              return <span style={{ color: "#52c41a" }}>有效</span>;
            })()}
          </Descriptions.Item>
          <Descriptions.Item label="有效期时长">
            {(() => {
              const days = Math.floor((new Date(cert.not_after).getTime() - new Date(cert.not_before).getTime()) / (24 * 3600 * 1000));
              const remain = Math.floor((new Date(cert.not_after).getTime() - Date.now()) / (24 * 3600 * 1000));
              return `${days} 天（剩余 ${remain <= 0 ? "已过期" : `${remain} 天`}）`;
            })()}
          </Descriptions.Item>
          <Descriptions.Item label="ca.cer">
            <Space>
              <Typography.Text code copyable>{(cert as unknown as { ca_path?: string | null }).ca_path ?? "-"}</Typography.Text>
              {(cert as unknown as { ca_path?: string | null }).ca_path && (
                <Button size="small" loading={downloading === "ca"} onClick={() => doDownload("ca")}>下载</Button>
              )}
            </Space>
          </Descriptions.Item>
          <Descriptions.Item label="fullchain.cer">
            <Space>
              <Typography.Text code copyable>{(cert as unknown as { fullchain_path?: string | null }).fullchain_path ?? cert.cert_pem_path}</Typography.Text>
              <Button size="small" loading={downloading === "fullchain"} onClick={() => doDownload("fullchain")}>下载</Button>
            </Space>
          </Descriptions.Item>
          <Descriptions.Item label="key">
            <Space>
              <Typography.Text code copyable>{(cert as unknown as { key_path?: string | null }).key_path ?? cert.key_pem_path}</Typography.Text>
              <Button size="small" loading={downloading === "key"} onClick={() => doDownload("key")}>下载</Button>
            </Space>
          </Descriptions.Item>
        </Descriptions>
      </Card>
    </div>
  );
}
