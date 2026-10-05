import { Typography } from "antd";

/** 占位页：后续任务逐个替换为真实实现（tasks 2.x/3.x）。 */
export function PlaceholderPage({ title }: { title: string }) {
  return (
    <Typography.Title level={3}>
      {title}
      <Typography.Text type="secondary" style={{ marginLeft: 12, fontSize: 14 }}>
        （建设中）
      </Typography.Text>
    </Typography.Title>
  );
}
