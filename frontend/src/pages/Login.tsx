import { useState } from "react";
import { useLocation, useNavigate } from "react-router";
import { App, Button, Card, Form, Input, Typography } from "antd";
import { SafetyCertificateOutlined } from "@ant-design/icons";
import { useAuthStore } from "@/auth/store";

interface LoginForm {
  username: string;
  password: string;
}

/** 品牌区：渐变图标 + 产品名 + 一句话定位，登录页与侧栏共用同一视觉语言。 */
function Brand() {
  return (
    <div style={{ textAlign: "center", marginBottom: 24 }}>
      <div
        style={{
          width: 52,
          height: 52,
          borderRadius: 14,
          margin: "0 auto 12px",
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          background: "linear-gradient(135deg, #4f63f2 0%, #7b5bf5 100%)",
          color: "#fff",
          fontSize: 26,
        }}
      >
        <SafetyCertificateOutlined />
      </div>
      <Typography.Title level={4} style={{ margin: 0 }}>
        acmecast 控制台
      </Typography.Title>
      <Typography.Text type="secondary">证书自动签发与部署</Typography.Text>
    </div>
  );
}

/** 登录页：换取 JWT 后回到来路（spec「登录成功进入控制台」）。 */
export function LoginPage() {
  const navigate = useNavigate();
  const location = useLocation();
  const { message } = App.useApp();
  const login = useAuthStore((state) => state.login);
  const [submitting, setSubmitting] = useState(false);

  const next = new URLSearchParams(location.search).get("next") ?? "/";

  const onFinish = async (values: LoginForm) => {
    setSubmitting(true);
    try {
      const response = await fetch("/api/login", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(values),
      });
      const body = await response.json();
      if (!response.ok) {
        const error = body?.error;
        // 统一错误呈现：不区分「用户名不存在」与「口令错误」（后端已保证）。
        message.error(error?.message ?? "登录失败");
        return;
      }
      login(body.data.token, values.username);
      navigate(decodeURIComponent(next), { replace: true });
    } catch (error) {
      message.error(`网络错误：${error instanceof Error ? error.message : error}`);
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div
      style={{
        minHeight: "100vh",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        background:
          "radial-gradient(1100px 500px at 70% -10%, rgba(79, 99, 242, 0.14), transparent 60%), radial-gradient(900px 500px at 10% 110%, rgba(123, 91, 245, 0.10), transparent 55%), #f4f6fb",
        padding: 24,
      }}
    >
      <Card
        style={{
          width: 380,
          boxShadow: "0 8px 30px rgba(28, 35, 51, 0.08)",
          borderRadius: 14,
        }}
        styles={{ body: { padding: 32 } }}
      >
        <Brand />
        <Form<LoginForm> onFinish={onFinish} layout="vertical" requiredMark={false}>
          <Form.Item
            name="username"
            label="用户名"
            rules={[{ required: true, message: "请输入用户名" }]}
          >
            <Input autoFocus size="large" />
          </Form.Item>
          <Form.Item
            name="password"
            label="口令"
            rules={[{ required: true, message: "请输入口令" }]}
          >
            <Input.Password size="large" />
          </Form.Item>
          <Button type="primary" htmlType="submit" block size="large" loading={submitting}>
            登录
          </Button>
        </Form>
      </Card>
    </div>
  );
}
