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

  const confirmRevoke = () => {
    modal.confirm({
      title: "确认吊销证书？",
      content: `将向 CA 发起吊销：${cert.domains.join(", ")}。吊销后证书立即失效，且不再参与自动续期。`,
      okText: "吊销",
      okButtonProps: { danger: true },
      cancelText: "取消",
      onOk: async () => {
        const result = await client.POST(`/api/certificates/${certId}/revoke`, {
          body: {},
        });
        if (result.error !== undefined) {
          const body = result.error as { error?: { message?: string } };
          message.error(body?.error?.message ?? "吊销失败");
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
          <Descriptions.Item label="证书文件">{cert.cert_pem_path}</Descriptions.Item>
          <Descriptions.Item label="私钥文件">{cert.key_pem_path}</Descriptions.Item>
        </Descriptions>
      </Card>
    </div>
  );
}
