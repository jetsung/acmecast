import { useState } from "react";
import { useLocation, useNavigate } from "react-router";
import { App, Button, Card, Form, Input, Typography } from "antd";
import { useAuthStore } from "@/auth/store";

interface LoginForm {
  username: string;
  password: string;
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
        background: "#f0f2f5",
      }}
    >
      <Card style={{ width: 360 }}>
        <Typography.Title level={4} style={{ textAlign: "center" }}>
          acmecast 控制台
        </Typography.Title>
        <Form<LoginForm> onFinish={onFinish} layout="vertical">
          <Form.Item
            name="username"
            label="用户名"
            rules={[{ required: true, message: "请输入用户名" }]}
          >
            <Input autoFocus />
          </Form.Item>
          <Form.Item
            name="password"
            label="口令"
            rules={[{ required: true, message: "请输入口令" }]}
          >
            <Input.Password />
          </Form.Item>
          <Button type="primary" htmlType="submit" block loading={submitting}>
            登录
          </Button>
        </Form>
      </Card>
    </div>
  );
}
