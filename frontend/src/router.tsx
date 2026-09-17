import { createBrowserRouter, Navigate } from "react-router";
import { RequireAuth } from "@/auth/RequireAuth";
import { AppLayout } from "@/layout/AppLayout";
import { LoginPage } from "@/pages/Login";
import { DashboardPage } from "@/pages/DashboardPage";
import { CertificateListPage } from "@/pages/certificates/CertificateListPage";
import { CertificateDetailPage } from "@/pages/certificates/CertificateDetailPage";
import { PipelineListPage } from "@/pages/pipelines/PipelineListPage";
import { PipelineEditorPage } from "@/pages/pipelines/PipelineEditorPage";
import { HistoryListPage } from "@/pages/histories/HistoryListPage";
import { HistoryDetailPage } from "@/pages/histories/HistoryDetailPage";
import { CredentialListPage } from "@/pages/credentials/CredentialListPage";
import { ScheduleConfigPage } from "@/pages/schedules/ScheduleConfigPage";

export const router = createBrowserRouter([
  { path: "/login", element: <LoginPage /> },
  {
    element: <RequireAuth />,
    children: [
      {
        element: <AppLayout />,
        children: [
          { path: "/", element: <DashboardPage /> },
          { path: "/certificates", element: <CertificateListPage /> },
          { path: "/certificates/:id", element: <CertificateDetailPage /> },
          { path: "/pipelines", element: <PipelineListPage /> },
          { path: "/pipelines/:id", element: <PipelineEditorPage /> },
          { path: "/histories", element: <HistoryListPage /> },
          { path: "/histories/:id", element: <HistoryDetailPage /> },
          { path: "/credentials", element: <CredentialListPage /> },
          { path: "/schedules", element: <ScheduleConfigPage /> },
          { path: "*", element: <Navigate to="/" replace /> },
        ],
      },
    ],
  },
]);
