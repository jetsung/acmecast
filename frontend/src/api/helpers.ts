import { ApiError } from "@/api/client";

/** 统一处理 openapi-fetch 的响应：失败时抛出携带后端错误结构的 ApiError。 */
export async function unwrap(result: {
  data?: unknown;
  error?: unknown;
  response: Response;
}): Promise<Record<string, unknown>> {
  if (result.error !== undefined) {
    const body = result.error as {
      error?: { code?: string; message?: string; field?: string };
    };
    throw new ApiError(result.response.status, body?.error);
  }
  return (result.data ?? {}) as Record<string, unknown>;
}

/** 把统一信封解包为 `data` 字段内容。 */
export async function unwrapData<T>(result: {
  data?: { data?: T } | undefined;
  error?: unknown;
  response: Response;
}): Promise<T> {
  const body = await unwrap(result);
  return body.data as T;
}

/** 下载二进制端点：读取 Content-Disposition 中的文件名并触发浏览器保存。 */
export async function downloadFile(url: string, token: string | null): Promise<void> {
  const response = await fetch(url, {
    headers: token ? { Authorization: `Bearer ${token}` } : undefined,
  });
  if (!response.ok) {
    const body = (await response.json().catch(() => undefined)) as
      | { error?: { code?: string; message?: string; field?: string } }
      | undefined;
    throw new ApiError(response.status, body?.error);
  }

  const disposition = response.headers.get("Content-Disposition") ?? "";
  const match = /filename="([^"]+)"/.exec(disposition);
  const filename = match?.[1] ?? "download.bin";

  const blob = await response.blob();
  const link = document.createElement("a");
  link.href = URL.createObjectURL(blob);
  link.download = filename;
  link.click();
  URL.revokeObjectURL(link.href);
}

/** 时间显示：本地时区的紧凑格式。 */
export function formatTime(iso: string | null | undefined): string {
  if (!iso) return "-";
  return new Date(iso).toLocaleString("zh-CN", { hour12: false });
}
