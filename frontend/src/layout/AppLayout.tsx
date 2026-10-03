import { Layout, Menu, Typography } from "antd";
import {
  DashboardOutlined,
  SafetyCertificateOutlined,
  PartitionOutlined,
  HistoryOutlined,
  KeyOutlined,
  ClockCircleOutlined,
  LogoutOutlined,
} from "@ant-design/icons";
import { Button } from "antd";
import { Outlet, useLocation, useNavigate } from "react-router";
import { useAuthStore } from "@/auth/store";

const { Sider, Content } = Layout;

/** 受保护区域的整体布局：侧边导航 + 内容区。 */
export function AppLayout() {
  const navigate = useNavigate();
  const location = useLocation();

  const items = [
    { key: "/", icon: <DashboardOutlined />, label: "仪表盘" },
    { key: "/certificates", icon: <SafetyCertificateOutlined />, label: "证书" },
    { key: "/pipelines", icon: <PartitionOutlined />, label: "流水线" },
    { key: "/histories", icon: <HistoryOutlined />, label: "运行历史" },
    { key: "/credentials", icon: <KeyOutlined />, label: "凭据" },
    { key: "/schedules", icon: <ClockCircleOutlined />, label: "调度" },
  ];

  const selected = items
    .filter((item) => location.pathname.startsWith(item.key) && item.key !== "/")
    .map((item) => item.key);
  if (selected.length === 0) {
    selected.push("/");
  }

  const logout = () => {
    // 与 401 自动登出共用同一 action：清除本地登录态后回登录页。
    useAuthStore.getState().logout();
    navigate("/login");
  };

  return (
    <Layout style={{ minHeight: "100vh" }}>
      <Sider>
        <div style={{ display: "flex", flexDirection: "column", height: "100%" }}>
          <div style={{ padding: 16 }}>
            <Typography.Title level={5} style={{ color: "#fff", margin: 0 }}>
              acmecast
            </Typography.Title>
          </div>
          <Menu
            theme="dark"
            mode="inline"
            selectedKeys={selected}
            items={items}
            onClick={(entry) => navigate(entry.key)}
          />
          <div style={{ marginTop: "auto", padding: 16 }}>
            <Button
              block
              danger
              ghost
              icon={<LogoutOutlined />}
              onClick={logout}
            >
              登出
            </Button>
          </div>
        </div>
      </Sider>
      <Content style={{ padding: 24 }}>
        <Outlet />
      </Content>
    </Layout>
  );
}
