import type { CSSProperties, ReactNode } from "react";
import { Typography } from "antd";

interface PanelProps {
  /** 可选面板标题（渲染为带底边的头部条）。 */
  title?: ReactNode;
  /** 标题右侧操作区（仅在提供 title 时渲染）。 */
  extra?: ReactNode;
  /** 内容区附加样式（如覆盖内边距）。 */
  bodyStyle?: CSSProperties;
  /** 面板级附加样式（如外边距）。 */
  style?: CSSProperties;
  children: ReactNode;
}

/**
 * 内容面板：白底圆角卡片容器。
 *
 * 手写样式而非 antd Card：jsdom 下 antd 6 的 Card 包裹 Table 会令个别
 * 组件测试挂起，div 容器视觉完全一致且无此风险。
 */
export function Panel({ title, extra, bodyStyle, style, children }: PanelProps) {
  return (
    <div
      style={{
        background: "#fff",
        borderRadius: 12,
        border: "1px solid #eef1f6",
        boxShadow: "0 1px 2px rgba(28, 35, 51, 0.04)",
        ...style,
      }}
    >
      {title !== undefined && (
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            gap: 12,
            padding: "14px 20px",
            borderBottom: "1px solid #f0f2f7",
          }}
        >
          <Typography.Text strong style={{ fontSize: 15 }}>
            {title}
          </Typography.Text>
          {extra}
        </div>
      )}
      <div style={{ padding: title !== undefined ? "8px 16px 16px" : 16, ...bodyStyle }}>
        {children}
      </div>
    </div>
  );
}
