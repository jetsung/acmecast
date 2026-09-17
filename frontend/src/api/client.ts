import createClient, { type Middleware } from "openapi-fetch";
import type { paths } from "./schema";
import { useAuthStore } from "@/auth/store";

export interface ApiErrorBody {
  code?: string;
  message?: string;
  field?: string;
}

/** 从统一错误信封解析出结构化错误；非信封响应退化为通用错误。 */
export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  readonly field?: string;

  constructor(status: number, body: ApiErrorBody | undefined) {
    super(body?.message ?? `请求失败（HTTP ${status}）`);
    this.status = status;
    this.code = body?.code ?? `http_${status}`;
    this.field = body?.field;
  }
}

/** 401 时的会话失效回调；由路由层注册（跳转登录页，design D3）。 */
let onUnauthorized: (() => void) | null = null;

export function setUnauthorizedHandler(handler: (() => void) | null) {
  onUnauthorized = handler;
}

export const client = createClient<paths>({
  baseUrl: "",
});

/** 统一中间件：请求注入 Bearer；401 时登出并触发跳转回调（design D3）。 */
const authMiddleware: Middleware = {
  onRequest({ request }) {
    const token = useAuthStore.getState().token;
    if (token) {
      request.headers.set("Authorization", `Bearer ${token}`);
    }
    return request;
  },
  onResponse({ response }) {
    if (response.status === 401) {
      useAuthStore.getState().logout();
      onUnauthorized?.();
    }
    return response;
  },
};

client.use(authMiddleware);
