import { lazy, Suspense, type ReactNode } from "react";
import { createBrowserRouter, Navigate, type RouteObject } from "react-router";
import { Spin } from "antd";
import { RequireAuth } from "@/auth/RequireAuth";
import { AppLayout } from "@/layout/AppLayout";

// 页面按路由懒加载：每个页面单独成一个 chunk，首屏只加载当前路由需要的那一份，
// 其余在导航时才拉取。页面都是具名导出，这里映射成 lazy 需要的 default。
//
// 外壳（RequireAuth / AppLayout / 懒加载占位用的 Spin）保持同步加载：它们是首屏
// 骨架，懒加载只会让每次导航先闪一下占位，反而更慢。
const LoginPage = lazy(() => import("@/pages/Login").then((m) => ({ default: m.LoginPage })));
const DashboardPage = lazy(() =>
  import("@/pages/DashboardPage").then((m) => ({ default: m.DashboardPage })),
);
const CertificateListPage = lazy(() =>
  import("@/pages/certificates/CertificateListPage").then((m) => ({
    default: m.CertificateListPage,
  })),
);
const CertificateDetailPage = lazy(() =>
  import("@/pages/certificates/CertificateDetailPage").then((m) => ({
    default: m.CertificateDetailPage,
  })),
);
const PipelineListPage = lazy(() =>
  import("@/pages/pipelines/PipelineListPage").then((m) => ({ default: m.PipelineListPage })),
);
const PipelineEditorPage = lazy(() =>
  import("@/pages/pipelines/PipelineEditorPage").then((m) => ({ default: m.PipelineEditorPage })),
);
const HistoryListPage = lazy(() =>
  import("@/pages/histories/HistoryListPage").then((m) => ({ default: m.HistoryListPage })),
);
const HistoryDetailPage = lazy(() =>
  import("@/pages/histories/HistoryDetailPage").then((m) => ({ default: m.HistoryDetailPage })),
);
const CredentialListPage = lazy(() =>
  import("@/pages/credentials/CredentialListPage").then((m) => ({ default: m.CredentialListPage })),
);
const ScheduleConfigPage = lazy(() =>
  import("@/pages/schedules/ScheduleConfigPage").then((m) => ({ default: m.ScheduleConfigPage })),
);

/** 懒加载期间的占位：内容区居中一个 spinner，侧边栏保持可见。 */
function RouteFallback() {
  return (
    <div style={{ display: "flex", justifyContent: "center", padding: 48 }}>
      <Spin size="large" />
    </div>
  );
}

/**
 * 给懒加载页面套上 Suspense。
 *
 * 边界放在**页面元素**这一层而不是 AppLayout 里：这样导航时只有内容区切到
 * 占位，侧边栏与用户已看到的框架不会整体卸载重挂。
 */
function route(node: ReactNode) {
  return <Suspense fallback={<RouteFallback />}>{node}</Suspense>;
}

/**
 * 路由表。
 *
 * 单独导出而不是只导出 `router`：测试要用 `createMemoryRouter(routes)` 在内存
 * 路由上验证懒加载，而 `createBrowserRouter` 依赖真实 history，无法在 jsdom 里
 * 指定初始路径。
 */
export const routes: RouteObject[] = [
  { path: "/login", element: route(<LoginPage />) },
  {
    element: <RequireAuth />,
    children: [
      {
        element: <AppLayout />,
        children: [
          { path: "/", element: route(<DashboardPage />) },
          { path: "/certificates", element: route(<CertificateListPage />) },
          { path: "/certificates/:id", element: route(<CertificateDetailPage />) },
          { path: "/pipelines", element: route(<PipelineListPage />) },
          { path: "/pipelines/:id", element: route(<PipelineEditorPage />) },
          { path: "/histories", element: route(<HistoryListPage />) },
          { path: "/histories/:id", element: route(<HistoryDetailPage />) },
          { path: "/credentials", element: route(<CredentialListPage />) },
          { path: "/schedules", element: route(<ScheduleConfigPage />) },
          { path: "*", element: <Navigate to="/" replace /> },
        ],
      },
    ],
  },
];

export const router = createBrowserRouter(routes);
