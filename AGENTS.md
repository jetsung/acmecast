> 给 AI 助手的实现思路参考（项目根目录，不进用户文档站）。

## 项目兼容性约定（重要）

**本项目是新项目，任何修改都不需要向上兼容。**

- 改 API、schema、数据库、配置文件、前端类型等时直接改，不必保留旧字段/旧接口的兼容层。
- 不要为了「老版本还能用」而保留废弃字段或写新旧并存分支。
- 唯一例外：数据库迁移文件按既有机制编写（保证空库→新库结构正确），无需支持旧库平滑升级。

## Schema 驱动表单（前端）

> 凭据页、流水线表单的字段布局、控件形态、联动行为由后端 schema 驱动，前端 `SchemaForm.tsx` 为通用渲染器，不为具体业务类型写分支。

### 渲染规则

- 控件选择：`string`→输入框、`enum`→下拉、`x-multiline`→`textarea`、`boolean`→`Switch`、`array<object>`→JSON 文本
- 布局宽度：标量默认半宽（`span 12`）两列一行；标注可改整行或行尾换行
- 联动：`x-required-when` / `x-visible-when` / `x-field-order` / `x-options`

### 布局决策表

| 需求效果 | 扩展 | 宽度 |
|---|---|---|
| 两个短字段同行 | 不标注（默认流式） | 半宽 |
| 字段独占一行且撑满 | `x-full-width: true` | 满行 |
| 多行文本（PEM/JSON） | `x-multiline: true` | 满行 textarea |
| 字段独占一行但只占半列 | `x-end-row: true`（配半宽） | 半宽 + 空占位列 |
| 字段渲染顺序 | 顶层 `x-field-order: [...]` | — |
| 条件必填 | 字段上 `x-required-when` | — |
| 条件显隐 | 字段上 `x-visible-when` | — |
| 资源 ID 选名称 | 调用方注入 `x-options` | — |
| 部署 config 随 target 变结构 | `x-target-schemas` | 满行 |

> `x-end-row` vs `x-full-width`：前者半宽+占位列（右侧留空），后者控件撑满两列（`width:100%`）。

### x-* 扩展实现位置

`frontend/src/components/SchemaForm.tsx` 有两条分支：顶层表单（`SchemaForm` 主组件）与部署目标子表单（`ObjectFieldGroup`）。给某分支加能力时需确认另一分支是否也需要。

| 扩展 | 顶层 | 子表单 |
|---|---|---|
| `x-multiline` | ✅ span 24 | ✅ span 24 |
| `x-full-width` | ✅ span 24 | ✅ span 24 |
| `x-end-row` | ✅ | ✅ |
| `x-field-order` | ✅ `orderedEntries` | — |
| `x-required-when` | ✅ | ✅ |
| `x-visible-when` | ✅ | ✅ |
| `x-target-schemas` | ✅ | — |

### 前端踩坑清单

1. **字母序重排**：后端 `properties` 按字母序产出，布局依赖顺序时需后端注入 `x-field-order`，前端按此顺序渲染。
2. **`allOf[$ref]` 包装**：具名枚举/结构体字段外层有 `allOf`，前端 `deref` 会合并外层扩展键，`span`/联动计算需读合并后节点。
3. **分支能力不对齐**：新增扩展前 `grep "x-xxx" SchemaForm.tsx` 确认两条分支都支持。
4. **断言 ant-col 姿势**：`closest(".ant-col")` 会命中 `Form.Item` 内层控制列，需先 `closest(".ant-form-item")` 再向外取列；断言 label 顺序时过滤掉外层「名称/类型」表单项。

## 本地调试

### 调试账号（开发环境）

- 用户名：`admin`
- 密码：`admin888`
- 生产环境通过 `ACMECAST_ADMIN_PASSWORD_HASH` / `ADMIN_PASSWORD` 覆盖。

### 使用 just 启动前、后端

`justfile` 的任务直接调用 `cargo` / `pnpm`（不再经 `mise exec` 包装）。工具链来源按以下优先级取用：全局已安装（`mise use -g`，用于本地所有项目共享）优先；未安装时回退到 `mise.toml` 声明的版本（进入仓库目录由 mise 自动激活，用于云端单项目隔离环境）。`.env` 由 `mise.toml` 的 `[env] _.file = ".env"` 自动加载，无需手动 export。首次使用：`cp .env.example .env`（详见 `docs/02-getting-started.md`）。

```bash
# 同时启动前后端（后端 0.0.0.0:8080，前端 localhost:5173，/api → 8080）
just dev

# 仅后端（可附加子命令，如生成口令哈希）
just backend
just backend hash-password 'admin888'

# 仅前端
just frontend          # pnpm -C frontend dev
just frontend-install  # 首次或 pnpm-lock.yaml 变更后

# 后端 OpenAPI 变更后刷新前端类型
just gen-api           # 需后端已在 8080 运行
```

> 切勿绕过 `just` 直接 `cargo run` / `pnpm dev`，否则 `mise` 与 `.env` 注入的 `JWT_SECRET` / `SWAGGER_UI_DOWNLOAD_URL` 等会丢失。
