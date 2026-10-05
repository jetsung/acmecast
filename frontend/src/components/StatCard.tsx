import type { ReactNode } from "react";
import { Card } from "antd";

interface StatCardProps {
  /** 指标名。 */
  title: string;
  /** 数值（数字或 `0 / 0` 之类的组合）。 */
  value: ReactNode;
  /** 数值颜色（如警示红）。 */
  valueColor?: string;
  /** 左侧彩色图标块。 */
  icon: ReactNode;
  /** 图标块底色（浅色）与内容色。 */
  tone?: { bg: string; color: string };
}

/**
 * 仪表盘统计卡：彩色图标块 + 数值。
 *
 * 用图标与色彩给指标建立视觉语义（正常/警示/危险），替代纯数字卡片的单调感。
 */
export function StatCard({ title, value, valueColor, icon, tone }: StatCardProps) {
  const resolvedTone = tone ?? { bg: "#eef0fe", color: "#4f63f2" };
  return (
    <Card styles={{ body: { display: "flex", alignItems: "center", gap: 16, padding: 20 } }}>
      <div
        style={{
          width: 46,
          height: 46,
          borderRadius: 12,
          background: resolvedTone.bg,
          color: resolvedTone.color,
          fontSize: 22,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          flex: "none",
        }}
      >
        {icon}
      </div>
      <div>
        <div style={{ fontSize: 13, color: "#5b6478", marginBottom: 2 }}>{title}</div>
        <div style={{ fontSize: 28, lineHeight: 1.2, fontWeight: 600, color: valueColor ?? "#1c2333" }}>
          {value}
        </div>
      </div>
    </Card>
  );
}
