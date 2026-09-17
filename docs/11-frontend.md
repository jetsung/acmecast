# 11. 前端控制台

控制台是 React 19 + TypeScript + Ant Design 6 的单页应用（SPA），镜像构建时自动打包，
产物由后端静态托管（`ACMECAST_STATIC_DIR=/static`），与 API 同源。

## 11.1 页面

| 路由 | 页面 | 能力 |
| --- | --- | --- |
| `/login` | 登录 | 输入用户名/口令，成功后保存令牌并跳回原地址 |
| `/` | 仪表盘 | 证书统计、最早到期的 5 张证书、最近运行记录 |
| `/certificates` | 证书列表 | 列表、到期着色、进入详情 |
| `/certificates/:id` | 证书详情 | 基本信息、吊销、多格式下载 |
| `/pipelines` | 流水线列表 | 列表、启用开关、**立即运行**、删除、进入编辑 |
| `/pipelines/new`、`/pipelines/:id` | 流水线编辑器 | 步骤增删/排序/启停，JSON Schema 驱动的动态表单 |
| `/histories` | 运行历史列表 | 按状态/来源查看，进入详情 |
| `/histories/:id` | 运行历史详情 | 终态、失败摘要、按步骤分组的日志 |
| `/credentials` | 凭据管理 | 列表、创建/编辑（Schema 表单）、连通性测试、删除 |
| `/schedules` | 调度配置 | 为流水线配置 cron、补跑、续期域名；显示已有调度 |

## 11.2 登录态

- 登录成功后令牌存入浏览器 `localStorage`（键 `acmecast-auth`），刷新可恢复；
- 任意请求返回 401 时统一登出；
- 令牌有效期默认 12 小时，过期需重新登录。

> 令牌存于 `localStorage` 存在 XSS 风险，当前按「单管理员、内网控制台」的定位接受
> 该取舍。生产建议把控制台置于受控网络，并注意反向代理的访问控制。

## 11.3 流水线编辑器

- 步骤类型来自 `GET /api/tasks`；
- 每种任务的输入表单由 `GET /api/tasks/{type_id}/schema` 返回的 JSON Schema
  **动态渲染**（支持 string / integer / number / boolean / enum / array<string> /
  一层嵌套 object）；
- **按字段语义选控件**：枚举（如 `challenge`）渲染成下拉；取值是资源 ID 或固定标识的
  字段也渲染成下拉——`account_credential_id` / `acme_account_credential_id` 列 ACME 账号
  凭据，`dns_credential_id` 列 DNS 提供商凭据，`dns_provider` 列内置提供商，
  `target` 列部署目标；值保持原类型（凭据 ID 仍是数字），无可选项时提示「暂无可选项」；
- 无类型字段（如 `cert.deploy` 的 `config`，结构随所选目标变化）按 **JSON 文本**编辑；
- 步骤可增删、上移下移、启用/停用；切换任务类型会清空该步骤的输入；
- 保存是**整体替换**：前端提交完整 `name` / `description` / `enabled` / `steps`。
  列表页的启用开关同样走「先取详情再整体 PUT」。

## 11.4 调度配置页

- 按流水线新建/编辑调度（同一流水线一份）；
- cron 输入会做**客户端语法预览**（使用 `cron-parser`，按 `Asia/Shanghai` 显示
  下一次触发时间）；
- 提交后由后端校验，非法表达式会被拒绝。

> **注意时区差异**：前端预览按东八区展示，而**后端实际按 UTC 触发**。
> 例如 `0 3 * * *` 前端预览为北京时间 03:00，实际触发是北京时间 11:00。
> 请以 UTC 换算填写，必要时把预览时间减去 8 小时核对。

## 11.5 凭据管理页

- 凭据类型下拉来自 `GET /api/credential-types`（当前仅 `acme.account`）；
- 字段表单同样由该类型返回的 JSON Schema 驱动；
- 「测试」按钮调用 `POST /api/credentials/{id}/test`，展示 `ok` / `unavailable`
  （含脱敏原因）/ `not_testable`；
- 删除被引用的凭据会被后端拒绝（409），页面会提示失败。

## 11.6 运行历史详情

状态为 `running` 时，页面每 5 秒自动轮询状态与日志，到达 `success`/`failed`
后停止——从流水线列表「立即运行」跳进来后无需手动刷新即可看到结果。

按步骤分组展示日志，级别有 `info` / `warn` / `error`。产物重名覆盖等异常会以
`warn` 形式出现在对应步骤的日志里——排查「部署的证书不是预期那份」时优先看这里。

## 11.7 开发前端

```bash
# 1. 先启动后端（默认 8080）
cargo run -p acmecast-server

# 2. 安装依赖并启动 dev server（/api 代理到 8080）
cd frontend
pnpm install
pnpm dev          # http://localhost:5173
```

可用脚本：

| 命令 | 作用 |
| --- | --- |
| `pnpm dev` | 启动 Vite 开发服务器 |
| `pnpm build` | 类型检查 + 生产构建（产物 `frontend/dist`） |
| `pnpm typecheck` | 仅类型检查（含测试文件与 vite/vitest 配置） |
| `pnpm test` | 跑前端用例（vitest + jsdom，覆盖路由懒加载、SchemaForm 各控件类型、流水线编辑器） |
| `pnpm test:e2e` | 跑端到端用例（Playwright + Chromium，覆盖 jsdom 测不了的布局） |
| `pnpm preview` | 预览已构建产物 |
| `pnpm gen:api` | 从 `http://127.0.0.1:8080/api/openapi.json` 重新生成 `src/api/schema.d.ts` |

> 后端接口变更后务必执行 `pnpm gen:api`，保持前端类型与 API 同步。

CI 的 `frontend` job（`.github/workflows/ci.yml`）按
`pnpm install --frozen-lockfile` → `pnpm test` → `pnpm build` →
`playwright install --with-deps chromium` → `pnpm test:e2e` 依次执行，
运行环境为 Node 24 + pnpm 11.20.0（与 `frontend/package.json` 的
`packageManager` 一致）。

### 端到端用例（11.7.1）

单测跑在 jsdom 里，**没有布局引擎**——antd `autoSize` 的行高在那里恒为单行，
量不出 `minRows` 的效果。这类只能靠真实浏览器验证的行为放在 `frontend/e2e/`：

- 用 `page.route` 桩掉全部 `/api/**`，**不需要后端**；
- 由 Playwright 的 `webServer` 先 `pnpm build` 再 `vite preview`（不用 dev server：
  dev 首次访问懒加载路由会触发依赖预构建并整页 reload，动态 import 会偶发失败）；
- 首次在本机跑需要装浏览器：`pnpm exec playwright install --with-deps chromium`。

## 11.8 生产托管

镜像已把 `frontend/dist` 复制到 `/static` 并设置 `ACMECAST_STATIC_DIR=/static`，
因此容器启动后直接访问根路径即可。源码部署时把 `ACMECAST_STATIC_DIR` 指向
`frontend/dist` 的绝对路径。

## 11.9 构建产物与拆包

构建做了两层拆分（`vite.config.ts` 的 `manualChunks` + 路由懒加载）：

**路由级懒加载**：每个页面单独成 chunk，导航到该路由时才拉取。外壳
（`RequireAuth`、`AppLayout`）与首屏骨架保持同步加载——懒加载外壳只会让每次
导航先闪占位，反而更慢。

**依赖按生态分块**（每组内部一起升级、一起失效，便于长期缓存）：

| chunk | 内容 |
| --- | --- |
| `index` | 应用入口与路由表 |
| `react` | react / react-dom / react-router / scheduler |
| `antd` | antd 本体 |
| `antd-primitives` | `@rc-component/*`、`rc-*` 与 `@ant-design/*`（样式引擎） |
| `antd-icons` | `@ant-design/icons` |
| `query` | `@tanstack/*` |
| `vendor` | 其余依赖 |
| 页面 chunk | 各页面及其**专属**依赖（如 `cron-parser` 随调度页、`openapi-fetch` 随首个页面） |

要点：

- **只被懒加载页面用到的依赖不并入 `vendor`**，否则它们会随入口一起加载，
  懒加载就白做了（`cron-parser` 约 30KB 会因此退回按需加载）；
- `@ant-design/*` 与 `rc-*` 互相引用，拆成两块会触发 rollup 的
  「circular chunk」告警，故合并为 `antd-primitives`；
- 每个 chunk 都低于 Vite 默认的 500KB 告警线。

效果（约数）：

- 入口 chunk 从 31KB 降到 **约 7KB**；
- 首屏 JS 由约 1.37MB 降到约 **1.31MB**（gzip 约 436KB → 422KB）——降幅有限是
  因为 antd/react 是骨架必须在首屏，页面代码本身只有几十 KB；
- 改动业务代码只会让入口 chunk 哈希变化，依赖 chunk 继续命中浏览器缓存；
- 页面与其专属依赖改为首次导航时才下载，各页面 chunk 多在 2–5KB。
