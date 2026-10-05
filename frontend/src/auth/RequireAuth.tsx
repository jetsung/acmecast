import { Navigate, Outlet, useLocation } from "react-router";
import { useAuthStore } from "@/auth/store";

/** 受保护路由组：未登录跳转登录页并记录来路（spec「登录与令牌管理」）。 */
export function RequireAuth() {
  const token = useAuthStore((state) => state.token);
  const location = useLocation();

  if (!token) {
    const next = encodeURIComponent(location.pathname + location.search);
    return <Navigate to={`/login?next=${next}`} replace />;
  }
  return <Outlet />;
}
