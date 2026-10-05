import type { ThemeConfig } from "antd";

/**
 * 全局设计令牌：控制台所有页面共享的视觉基调。
 *
 * 品牌色取靛蓝（indigo）系——证书/TLS 产品的信任感与工具属性；背景、圆角、
 * 侧边菜单在此集中定义，避免每个页面各调各的造成观感漂移。
 */
export const themeConfig: ThemeConfig = {
  token: {
    colorPrimary: "#4f63f2",
    colorInfo: "#4f63f2",
    colorLink: "#4f63f2",
    borderRadius: 8,
    colorBgLayout: "#f4f6fb",
    fontFamily:
      "-apple-system, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', sans-serif",
    colorTextHeading: "#1c2333",
  },
  components: {
    Layout: {
      siderBg: "#ffffff",
      headerBg: "#ffffff",
      headerHeight: 56,
      headerPadding: "0 24px",
    },
    Menu: {
      itemBg: "transparent",
      itemSelectedBg: "#eef0fe",
      itemSelectedColor: "#4f63f2",
      itemHoverBg: "#f5f6fa",
      itemBorderRadius: 8,
      itemMarginInline: 10,
      itemMarginBlock: 4,
      iconMarginInlineEnd: 12,
      subMenuItemBg: "transparent",
    },
    Card: {
      borderRadiusLG: 12,
      boxShadowTertiary: "0 1px 2px rgba(28, 35, 51, 0.04)",
    },
    Table: {
      headerBg: "#fafbfe",
      headerColor: "#5b6478",
      headerSplitColor: "transparent",
      cellPaddingBlock: 14,
      borderRadius: 12,
    },
    Statistic: {
      titleFontSize: 13,
      contentFontSize: 28,
    },
    Button: {
      fontWeight: 500,
    },
  },
};
