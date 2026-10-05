import type { ReactNode } from "react";
import { Space, Typography } from "antd";

interface PageHeaderProps {
  /** 页面主标题。 */
  title: ReactNode;
  /** 标题右侧的补充说明（如计数徽标）。 */
  subtitle?: ReactNode;
  /** 标题下方的辅助描述。 */
  description?: ReactNode;
  /** 页面级操作区，渲染在右侧。 */
  extra?: ReactNode;
}

/**
 * 页面统一头部：左侧标题（含说明）+ 右侧操作区。
 *
 * 替代各页面手写的 `Typography.Title` + 散排按钮，保证标题层级、
 * 间距与操作位置在所有页面一致。
 */
export function PageHeader({ title, subtitle, description, extra }: PageHeaderProps) {
  return (
    <div
      style={{
        display: "flex",
        alignItems: "flex-start",
        justifyContent: "space-between",
        gap: 16,
        marginBottom: 20,
        flexWrap: "wrap",
      }}
    >
      <div>
        <Space align="center" size={10}>
          <Typography.Title level={4} style={{ margin: 0 }}>
            {title}
          </Typography.Title>
          {subtitle}
        </Space>
        {description && (
          <Typography.Text type="secondary" style={{ display: "block", marginTop: 4 }}>
            {description}
          </Typography.Text>
        )}
      </div>
      {extra && <Space size={8}>{extra}</Space>}
    </div>
  );
}
