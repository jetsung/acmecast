import { Avatar, Button, Layout, Menu, Tooltip, Typography } from "antd";
import {
  DashboardOutlined,
  SafetyCertificateOutlined,
  PartitionOutlined,
  HistoryOutlined,
  KeyOutlined,
  ClockCircleOutlined,
  LogoutOutlined,
} from "@ant-design/icons";
import { Outlet, useLocation, useNavigate } from "react-router";
import { useAuthStore } from "@/auth/store";

const { Sider, Header, Content } = Layout;

/** 导航项集中定义：侧栏菜单与顶栏标题共用，避免两处漂移。 */
const NAV_ITEMS = [
  { key: "/", icon: <DashboardOutlined />, label: "仪表盘" },
  { key: "/certificates", icon: <SafetyCertificateOutlined />, label: "证书" },
  { key: "/pipelines", icon: <PartitionOutlined />, label: "流水线" },
  { key: "/histories", icon: <HistoryOutlined />, label: "运行历史" },
  { key: "/credentials", icon: <KeyOutlined />, label: "凭据" },
  { key: "/schedules", icon: <ClockCircleOutlined />, label: "调度" },
];

/** 品牌区：图标方块 + 产品名，替代原先的一行白字。 */
function Brand() {
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 10, padding: "16px 14px 14px" }}>
      <div
        style={{
          width: 34,
          height: 34,
          borderRadius: 10,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          background: "linear-gradient(135deg, #4f63f2 0%, #7b5bf5 100%)",
          color: "#fff",
          fontSize: 18,
          flex: "none",
        }}
      >
        <SafetyCertificateOutlined />
      </div>
      <div style={{ lineHeight: 1.2 }}>
        <div style={{ fontWeight: 600, fontSize: 16, color: "#1c2333" }}>acmecast</div>
        <div style={{ fontSize: 12, color: "#8a93a6" }}>证书自动化控制台</div>
      </div>
    </div>
  );
}

/** 受保护区域的整体布局：浅色侧边导航 + 顶栏用户区 + 内容画布。 */
export function AppLayout() {
  const navigate = useNavigate();
  const location = useLocation();
  const username = useAuthStore((state) => state.username);

  const selected = NAV_ITEMS.filter(
    (item) => location.pathname.startsWith(item.key) && item.key !== "/",
  ).map((item) => item.key);
  if (selected.length === 0) {
    selected.push("/");
  }
  const current = NAV_ITEMS.find((item) => item.key === selected[0]);

  const logout = () => {
    // 与 401 自动登出共用同一 action：清除本地登录态后回登录页。
    useAuthStore.getState().logout();
    navigate("/login");
  };

  return (
    <Layout style={{ minHeight: "100vh" }}>
      <Sider width={216} style={{ borderRight: "1px solid #edf0f7" }}>
        <Brand />
        <Menu
          mode="inline"
          selectedKeys={selected}
          items={NAV_ITEMS}
          onClick={(entry) => navigate(entry.key)}
          style={{ borderInlineEnd: "none", paddingBlock: 4 }}
        />
      </Sider>
      <Layout>
        <Header
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            borderBottom: "1px solid #edf0f7",
            position: "sticky",
            top: 0,
            zIndex: 10,
          }}
        >
          <Typography.Text strong style={{ fontSize: 15 }}>
            {current?.label}
          </Typography.Text>
          <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
            <Avatar size={28} style={{ background: "#4f63f2", fontSize: 14 }}>
              {(username ?? "A").slice(0, 1).toUpperCase()}
            </Avatar>
            <Typography.Text type="secondary">{username ?? "admin"}</Typography.Text>
            <Tooltip title="登出">
              <Button
                type="text"
                aria-label="登出"
                icon={<LogoutOutlined style={{ color: "#8a93a6" }} />}
                onClick={logout}
              />
            </Tooltip>
          </div>
        </Header>
        <Content style={{ padding: 24 }}>
          <div style={{ maxWidth: 1240, margin: "0 auto" }}>
            <Outlet />
          </div>
        </Content>
      </Layout>
    </Layout>
  );
}
