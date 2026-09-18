import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import path from "node:path";

/** 把模块 id 归一成 node_modules 里的包名（`@scope/name` 或 `name`）。 */
function packageOf(id: string): string | undefined {
  const marker = "node_modules/";
  const at = id.lastIndexOf(marker);
  if (at === -1) return undefined;
  const rest = id.slice(at + marker.length);
  if (rest.startsWith("@")) {
    return rest.split("/").slice(0, 2).join("/");
  }
  return rest.split("/")[0];
}

/**
 * 第三方依赖按「生态」拆成独立 chunk。
 *
 * 不拆的话所有依赖挤在入口 chunk（约 1.4MB），任何一次业务代码改动都会让
 * 浏览器重新下载整个依赖树。拆开后：依赖 chunk 只随依赖版本变化，业务代码
 * 改动时可长期命中缓存；各 chunk 可并行下载；每块都落在 500KB 警戒线以下，
 * 体积告警不再被当作噪音忽略。
 *
 * 分组按「一起升级、一起失效」划线：react 生态、antd、antd 图标、antd 原语、
 * 数据层各成一块，其余归入 vendor。
 */
/** 只被懒加载页面用到的依赖：不并入 vendor，随页面按需加载。 */
const LAZY_ONLY_PACKAGES = new Set(["cron-parser", "openapi-fetch"]);

function manualChunks(id: string): string | undefined {
  const pkg = packageOf(id);
  if (pkg === undefined) return undefined;

  if (
    pkg === "react" ||
    pkg === "react-dom" ||
    pkg === "react-router" ||
    pkg === "react-router-dom" ||
    pkg === "scheduler"
  ) {
    return "react";
  }

  // antd 一起打包会到 850KB 以上，按「图标 / 样式引擎 / 组件原语 / antd 本身」
  // 再切一刀，每块才落回警戒线内。
  if (pkg === "antd") {
    return "antd";
  }
  if (pkg.startsWith("@ant-design/icons")) {
    return "antd-icons";
  }
  // 样式引擎（@ant-design/cssinjs 等）与组件原语（@rc-component/*、rc-*）
  // 互相引用，拆成两块会得到 rollup 的「circular chunk」告警，因此并入同一块。
  if (
    pkg.startsWith("@ant-design/") ||
    pkg.startsWith("@rc-component/") ||
    pkg.startsWith("rc-")
  ) {
    return "antd-primitives";
  }

  if (pkg.startsWith("@tanstack/")) {
    return "query";
  }

  // 只被懒加载页面引用的库交给 rollup 默认分组，跟着页面 chunk 一起按需加载；
  // 兜进 vendor 会让它们随入口加载，懒加载就白做了。
  if (LAZY_ONLY_PACKAGES.has(pkg)) {
    return undefined;
  }

  // 其余依赖统一进 vendor：它们多被多个 chunk 共享，散落给默认分组会在
  // antd 各组之间形成「circular chunk」（rollup 的告警，且初始化顺序敏感）。
  return "vendor";
}

// 开发期把 /api 代理到本地后端（D10）：同源路径，生产同源部署同样无 CORS。
export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "src"),
    },
  },
  build: {
    rollupOptions: {
      output: {
        manualChunks,
      },
    },
  },
  server: {
    // 允许通过自定义域名（内网穿透 / 反向代理）访问 dev server，
    // 否则 Vite 会以 "Blocked request ... not allowed" 拒绝非 localhost 的 Host。
    allowedHosts: true,
    host: true,
    proxy: {
      "/api": {
        target: "http://127.0.0.1:8080",
        changeOrigin: false,
      },
    },
  },
});
