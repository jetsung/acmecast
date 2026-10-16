import type { components } from "@/api/schema";
import { client } from "@/api/client";
import { unwrapData } from "@/api/helpers";

/** 通知渠道（脱敏视图，不含 url/secret 等投递字段）。 */
export type NotificationChannel = components["schemas"]["NotificationChannelResponse"];

/**
 * 拉取已声明的通知渠道列表（含未启用的渠道，供用户辨识「配了但没投递」）。
 * 用作 `notify.summary` 步骤 `channels` 字段的多选选项。
 */
export async function listNotificationChannels(): Promise<NotificationChannel[]> {
  const result = await client.GET("/api/notifications/channels", {});
  return unwrapData<NotificationChannel[]>(result);
}
