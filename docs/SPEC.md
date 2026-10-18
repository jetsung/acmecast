# AcmeCast 项目规范（SPEC）

本文件为全局规范，涵盖品牌与命名、控制台站点图标与 PWA Manifest、ACME 账号、DNS 校验与证书生命周期、凭据出网代理与全局代理配置、运行历史可观测性等产品行为，`docs/SPEC.md` 为唯一真实来源。下文首节为品牌与命名规范，次节为控制台站点图标与 PWA Manifest（`console-favicon`），再次为 DNS Zone 推导，其余为内建 CA、凭据代理、日志规范等全局约定。
## 品牌与命名规范

用户可见品牌名统一为 `AcmeCast`（首字母 `A` 与 `C` 大写，其余小写），技术标识符保持 `acmecast` / `ACMECAST` 小写形态，二者以白名单式豁免边界划分，不做全局正则批量替换，也不引入技术标识符的破坏性变更。若未来需更名技术标识符，另起变更。

### 展示层品牌名为 AcmeCast

用户可见的品牌字样 MUST 显示为 `AcmeCast`，不出现全小写 `acmecast` 作为展示用品牌名。适用范围：

| 触点 | 期望文案 | 备注 |
|---|---|---|
| 前端浏览器标题 `frontend/index.html` `<title>` | `AcmeCast 控制台` | 构建产物 `dist/index.html` 同步 |
| 侧边栏品牌区 `frontend/src/layout/AppLayout.tsx` Brand 组件 | `AcmeCast` | 副文案 `证书自动化控制台` 保持不变，样式（渐变图标、字号、色值）不变 |
| 登录页品牌标题 `frontend/src/pages/Login.tsx` | `AcmeCast 控制台` | 副标题 `证书自动签发与部署` 保持不变 |
| README 首屏 `README.md` | 一级标题 `# AcmeCast`，正文首提 `AcmeCast` | 技术标识符 `ACMECAST_*` / `ghcr.io/jetsung/acmecast` / 路径保持原文 |
| 文档站 `docs/index.md` 与 `site/` 搜索/页面文案 | 站点标题与首段为 `AcmeCast` | `site/search.json` 为构建产物，改源后需重建 |
| OpenAPI `crates/acmecast-server/src/openapi.rs` `info.title` | `AcmeCast API` | Swagger UI 与 `openapi-typescript` 生成的客户端直接可见 |
| 面向用户的日志与错误提示中的品牌称呼 | `AcmeCast` | 技术标识符上下文除外 |

前端断言同步：`frontend/src/router.test.tsx` 中对品牌文案的断言 MUST 与上述展示保持一致（`AcmeCast 控制台`）。

### 技术标识符保持小写形态

以下技术标识符 MUST 保持现有小写/全大写下划线形态，不随展示层更名而变更，不新增大小写变体：

| 类别 | 保持形态 | 示例 |
|---|---|---|
| Rust crate 名（11 个） | `acmecast-*` 小写 | `acmecast-core`、`acmecast-server`、`acmecast-dns` 等 11 个内部 crate |
| 环境变量前缀 | `ACMECAST__` / `ACMECAST_*` 全大写下划线 | `ACMECAST__JWT_SECRET`、`ACMECAST_CONFIG`、`ACMECAST_NETWORK_TIMEOUT_SECS`、`ACMECAST_DOH_RESOLVERS` |
| 二进制名 | `acmecast-server` 小写 | — |
| Docker 镜像 | `ghcr.io/jetsung/acmecast` 小写 | 含容器名 |
| 仓库路径 | `github.com/jetsung/acmecast` 小写 | — |
| npm 包名 | `acmecast-frontend` 小写 | — |
| User-Agent | `acmecast/<version>` 小写 | 出站请求头 |
| CLI 可执行名前缀 | `acmecast-*` 小写 | — |

### 配置与数据路径保持小写

配置文件与数据文件路径、示例中的小写标识 MUST 保持不变，不因展示层更名而改为 `AcmeCast` 路径或大小写变体：

| 路径/标识 | 保持形态 |
|---|---|
| 默认配置查找顺序 | `./config.yaml` → `./config.yml` → `/etc/acmecast/config.yaml` |
| 示例数据 | `data/acmecast.db`、`data/config.toml` 模板路径 |
| User-Agent | `acmecast/<version>` |

### 文档与站点品牌提及统一

`README.md`、`docs/index.md`、`site/` 搜索与页面文案、`docker/compose.yaml` 注释、`mise.toml` / `prek.toml` / `data/config.toml` 模板注释中面向读者的品牌提及 MUST 使用 `AcmeCast`，技术标识符（crate 名、环境变量、路径、镜像名）在注释中的原文保持小写豁免，不被批量替换。判定规则：注释中若品牌词后紧跟技术标识符示例（如 `ACMECAST_JWT_SECRET`），仅改品牌称呼部分，技术标识符原文豁免。

### 本地存储与内部注释豁免

以下 MUST 保持小写不变，不因展示层更名而迁移或重命名，避免引入存储迁移与无意义重命名：

| 豁免项 | 保持形态 | 说明 |
|---|---|---|
| 前端本地存储 key | `acmecast-auth` (`frontend/src/auth/store.ts`) | 会话不因更名丢失，无需迁移脚本 |
| 源码内部注释中对 crate/模块路径的引用 | `acmecast-core` 等 | 保持小写原文 |
| 测试中临时文件名前缀 | `acmecast-*` | 保持小写 |

### 实现索引

- `frontend/index.html` — `<title>AcmeCast 控制台</title>`
- `frontend/src/layout/AppLayout.tsx` — 侧边栏 Brand 组件文本 `AcmeCast`
- `frontend/src/pages/Login.tsx` — 登录页品牌标题 `AcmeCast 控制台`
- `frontend/src/router.test.tsx` — 品牌文案断言同步
- `frontend/src/auth/store.ts` — `acmecast-auth` 本地存储 key 豁免（保持小写）
- `crates/acmecast-server/src/openapi.rs` — `#[openapi(info(title = "AcmeCast API"))]`
- `README.md` — 一级标题 `# AcmeCast` 与首段品牌提及
- `docs/index.md` / `site/search.json` — 站点标题与搜索文案 `AcmeCast`（`site/` 需重建）
- `docker/compose.yaml` / `mise.toml` / `prek.toml` / `data/config.toml` — 模板注释中面向读者的品牌称呼 `AcmeCast`，技术标识符原文豁免
- 技术标识符保留清单：`Cargo.toml` 中 11 个 `acmecast-*` crate、`ACMECAST__*` / `ACMECAST_*` 环境变量、`acmecast-server` 二进制、`ghcr.io/jetsung/acmecast` 镜像、`acmecast/<version>` User-Agent


## 控制台站点图标与 PWA Manifest（console-favicon）

为 AcmeCast 控制台提供浏览器标签页、书签、移动端主屏的站点图标与 PWA 可安装能力，基于登录页品牌标识一次性生成多尺寸 favicon 与 Web App Manifest，并在前端入口与后端静态托管中建立可验证的引用契约。`frontend/public` 为运行时权威副本，Vite 将其 verbatim 拷贝至 `frontend/dist` 根目录，随 Docker `frontend-builder → /static` 由后端 `ServeDir` 托管。

### Favicon 静态资源清单

`frontend/public` MUST 包含 favicon 静态资源集合，并 verbatim 拷贝至 `frontend/dist` 根目录与运行期 `/static`。集合 MUST 包含：

| 资源 | 规格 | 说明 |
|---|---|---|
| `favicon.ico` | 含 16×16、32×32、48×48 三档 | 多尺寸 ICO，兼容旧浏览器/书签自动探测 |
| `favicon-32x32.png` | 32×32 PNG | 现代浏览器首选 |
| `favicon-16x16.png` | 16×16 PNG | 现代浏览器首选 |
| `apple-touch-icon.png` | 180×180 PNG | iOS 主屏/高分屏 |
| `site.webmanifest` | 合法 JSON（见下节） | PWA 可安装元数据 |

所有位图 MUST 由 LOGO 源派生并提交至仓库；派生 MUST 经仓库脚本 `scripts/generate-favicon.sh`（`just favicon <图片路径> [输出目录]`）完成，该脚本支持 `png`/`jpg`/`svg` 输入且同一输入多次运行输出字节一致（`-strip` 去元数据、ICO 目录项按 16/32/48 排序、SVG 光栅化统一 8-bit 色深）。构建链路（`pnpm -C frontend build` / Docker `frontend-builder`）不执行生成，仅拷贝已提交的成品。

- 构建产物一致性：执行 `pnpm -C frontend build` 后 `frontend/dist` 下上述 5 文件 MUST 存在且与 `frontend/public` 对应文件字节一致。
- ICO 多尺寸：以二进制解析 `favicon.ico`，目录项 MUST 至少包含 16、32、48 三档且每档可独立解码为有效位图。
- Apple Touch Icon：`apple-touch-icon.png` 图像头 MUST 为 180×180、色深 ≥ 24 位，不得为 32×32 拉伸放大图；透明背景与圆角留白保留。

### HTML 入口声明

`frontend/index.html` 的 `<head>` MUST 声明 favicon 与 PWA manifest 引用，声明 MUST 包含且 `href` MUST 为站点根路径绝对路径（`/` 前缀），不得使用相对路径或带 hash 的构建产物路径，确保在任意前端路由（`/pipelines`、`/certificates/xxx`、`/login` 等）下均可解析：

```html
<link rel="icon" href="/favicon.ico" sizes="any">
<link rel="icon" type="image/png" sizes="32x32" href="/favicon-32x32.png">
<link rel="icon" type="image/png" sizes="16x16" href="/favicon-16x16.png">
<link rel="apple-touch-icon" href="/apple-touch-icon.png">
<link rel="manifest" href="/site.webmanifest">
<meta name="theme-color" content="#4f63f2">
```

- 声明与资源一一对应：`index.html` 中每个 `href` 去掉前导 `/` 后 MUST 对应 `frontend/public` 下真实文件存在；逐项请求 MUST 返回 `200` 且 `Content-Type` 正确（`favicon.ico` 为 `image/x-icon` 或 `image/vnd.microsoft.icon`，PNG 为 `image/png`，`site.webmanifest` 为 `application/manifest+json` 或 `application/json`）。
- `sizes="any"` 使现代浏览器优先选 PNG、旧浏览器回落 ICO；`theme-color` 固定为品牌主色 `#4f63f2`。

### PWA Web App Manifest

`frontend/public/site.webmanifest` MUST 为合法 JSON，MUST 包含以下字段且满足浏览器可安装性校验（Lighthouse/DevTools 不报缺失 icon 或 start_url）：

```json
{
  "name": "AcmeCast",
  "short_name": "AcmeCast",
  "start_url": "/",
  "display": "standalone",
  "background_color": "#ffffff",
  "theme_color": "#4f63f2",
  "icons": [
    { "src": "/favicon-32x32.png", "sizes": "32x32", "type": "image/png" },
    { "src": "/apple-touch-icon.png", "sizes": "180x180", "type": "image/png" }
  ]
}
```

- `name`/`short_name` MUST 均为 `AcmeCast`；`icons` MUST 至少包含 32×32 与 180×180 两档，`src` 指向 `/favicon-32x32.png` 与 `/apple-touch-icon.png` 且 `type` 为 `image/png`，`sizes` 与实际 PNG 资源一致。
- `start_url` MUST 为 `/`；`display` MUST 为 `standalone` 或 `minimal-ui`。
- `theme_color` MUST 为 `#4f63f2`，`background_color` MUST 为 `#ffffff`。
- 不实现 Service Worker、离线缓存与推送；PWA 仅提供 Manifest 层可安装元数据。不引入 SVG favicon / `mask-icon`。

### 后端静态托管可达性

后端对 `ACMECAST_STATIC_DIR`（Docker 镜像中为 `/static`，由 `frontend/dist` 拷贝而来）的静态托管 MUST 使 favicon 资源通过与前端同源的 HTTP 路径可达，且不得被 SPA fallback 误判为前端路由。`ServeDir::new(static_dir).fallback(ServeFile::new(index))` 的行为 MUST 满足：当请求路径命中 `static_dir` 下的真实文件时返回该文件，仅当文件不存在时才回落到 `index.html`。

- 直接请求：`GET /favicon.ico`、`GET /favicon-32x32.png`、`GET /apple-touch-icon.png`、`GET /site.webmanifest` MUST 返回 `200`，`Content-Type` 分别为图标/图像/manifest 类型，响应体与 `frontend/dist` 下对应文件一致，且不返回 `index.html` 的 HTML 内容。
- SPA 回落不遮蔽：`GET /non-existent-route` 返回 `index.html`（SPA fallback），而 `GET /favicon.ico` 仍返回 ICO 文件，二者行为可区分。
- 浏览器自动探测兜底：未解析 `index.html` 而直接探测 `GET /favicon.ico` 时仍返回 `200` 的 ICO 文件，不依赖 HTML 声明。
- `Content-Type` 与可缓存响应头由 `ServeDir` 默认或显式配置提供，至少允许浏览器缓存；`site.webmanifest` 不被 `fallback` 吞没。

### 视觉一致性与源同步

所有 favicon 派生图 MUST 与源 LOGO 在视觉上一致：图形主体、品牌色、圆角/留白比例保持一致，不得引入额外文字或边框。源 LOGO 为登录页登录框上方的品牌标识（`frontend/src/pages/Login.tsx` 的 `Brand` 组件：渐变圆角方块 `#4f63f2 → #7b5bf5` 135° 对角渐变 + 白色盾形对勾），`frontend/public` 下的 favicon 为控制台运行时权威副本。

- 后续 LOGO 变更时 MUST 先更新 `Brand` 视觉（或按 `openspec/changes/add-favicon-from-logo/logo-prompt.md` 的提示词生成新 SVG/PNG 源），再经 `just favicon <新图路径>` 一次性重新生成派生尺寸并提交，`frontend/dist` 构建产物同步更新。
- 生成规范（见 `logo-prompt.md`）：圆角方形充满整张画布、尖角透明（alpha 非白底）、135° 渐变 `#4f63f2 → #7b5bf5`、白色描边盾牌 + 对勾、扁平极简无阴影/文字。

### 可验证性与回归防护

favicon 能力 MUST 可被自动化验证，避免后续重构或路由变更导致图标丢失：

- 构建产物校验：`frontend/public` 下任一必需文件缺失后执行构建，`frontend/dist` 中对应文件亦缺失，可被 CI 中的产物存在性断言捕获。
- 后端冒烟：对 `GET /favicon.ico` / `GET /favicon-32x32.png` / `GET /apple-touch-icon.png` / `GET /site.webmanifest` 的集成/冒烟测试 MUST 断言 `200` 与正确 `Content-Type`，`site.webmanifest` 额外断言 JSON 合法且 `name`/`icons`/`start_url` 完整；未正确挂载 `ACMECAST_STATIC_DIR` 或 `ServeDir` 配置错误时测试失败。
- 前端断言：任意路由下 `document.querySelector('link[rel="icon"]')`、`link[rel="apple-touch-icon"]`、`link[rel="manifest"]` 与 `meta[name="theme-color"]` 存在且 `href` 以 `/` 开头。

### 实现索引

- `frontend/public/favicon.ico` / `favicon-32x32.png` / `favicon-16x16.png` / `apple-touch-icon.png` / `site.webmanifest` — favicon 静态资源权威副本（提交产物）
- `frontend/index.html` — `<head>` 内 favicon/manifest/`theme-color` 声明（`/` 绝对路径）
- `frontend/dist/*` — Vite verbatim 拷贝产物（构建后与 `public` 字节一致）
- `scripts/generate-favicon.sh` + `justfile: favicon` — 派生重生成脚本（支持 png/jpg/svg、字节可复现，不进构建链路）
- `openspec/changes/add-favicon-from-logo/logo-prompt.md` — LOGO 重生成提示词（圆角方形/透明尖角/渐变/盾牌对勾）
- `crates/acmecast-server/src/lib.rs: ServeDir::new(static_dir).fallback(ServeFile::new(index))` — 静态托管（文件命中优先、缺失回落 `index.html`，无需代码改动）
- `docker/Dockerfile: frontend-builder → /static` — 前端构建产物拷贝至运行镜像
- `frontend/src/pages/Login.tsx: Brand` — LOGO 源（渐变圆角方块 `#4f63f2 → #7b5bf5` + 白色盾形对勾）

## DNS Zone 自动推导

流水线 `cert.apply` 的 `dns_zone` 为空时按本节推导；无法推导或多域时置为必填。

### 术语

- `domains`：流水线输入的域名列表，元素为 `example.com` 或 `*.example.com` 通配形式。
- `dns_zone`：挑战记录写入的托管区域名称，用于构造 `_acme-challenge.<dns_zone>` 的写入位置。
- `dns_alias`：domain-alias 模式下挑战记录的写入名（任意 FQDN，不带 `_acme-challenge.` 前缀），填写时 `dns_zone` 必填。留空走 challenge-alias / 同 zone 模式。

### 规则

#### R1 通配符单域
当 `domains` 仅含一个元素且为 `*.xxx.xx` 形式时，`dns_zone` 缺省为剥离前缀 `*.` 后剩余的合法 zone。

- 输入 `*.gcli.cn` → 推导 `gcli.cn`
- 输入 `*.example.com` → 推导 `example.com`
- 输入 `*.x.idev.top` → 候选逐级为 `x.idev.top`、`idev.top`，取提供商侧首个已托管者；若均无，则回退 `x.idev.top` 并由创建阶段报错（不在此处吞错）。
- 若剥离后非合法 zone（如 `*.com`、`*.`、`*.example.`），视为无法推导，按 R4 处理。

#### R2 非通配三级及以上单域
当 `domains` 仅含一个元素且非通配、形如 `xx.xxx.xx`（三级及以上）时，`dns_zone` 缺省为去除第一个点前的前缀后剩余部分。

- 输入 `a.example.com` → `example.com`
- 输入 `a.b.example.com` → `b.example.com`
- 仅当剩余仍含点时有效（确保至少两段），否则按 R4 处理。

#### R3 单域且本身为二级域名
当 `domains` 仅含一个元素且本身就是二级域名（`xxx.xx`，恰含一个点）时，`dns_zone` 即其本身。

- 输入 `skiy.net`、`example.com` → 推导自身 `skiy.net`、`example.com`

#### R4 无法推导或多域必填
当无法通过 R1–R3 推导（如 `*.com`、空输入、非合法形态）或 `domains` 含多个域名时，`dns_zone` 为必填，缺失则在校验或执行期报错：

```
无法从 <domain> 推导 DNS zone，请显式配置 dns_zone
```

`dns_zone` 的必填条件为「`domains` 至少 2 项」与「`dns_alias` 非空白」的**并集**（命中任一即必填）。前端 `cert.apply` 的 `dns_zone` 字段 schema 携带条件必填标记（`x-required-when`），使前端能据此联动校验；描述 MUST 说明显式配置的适用场景。

#### R5 单域推导的通用口径
若 `domains` 为单域且 `dns_zone` 未设置，推导口径为“多域名中的域名去除第一个点前的前缀”：

- 非通配：取第一个点后的剩余（`xx.xxx.xx` → `xxx.xx`）
- 通配：等同 R1 的“去 `*.`”（`*.xxx.xx` → `xxx.xx`）
- 该值须为合法 zone，否则按 R4 报错。

#### R6 跨 zone 委托
当挑战记录写入位置与 CA 侧查询名分离（`_acme-challenge.<dns_zone>` 与 `_acme-challenge.<授权域名>` CNAME 委派），`dns_zone` 按托管实际位置填写，提供商侧写入名由 `challenge_write_name(_acme-challenge.<dns_zone>)` 与 CA 查询名 `challenge_record_name(_acme-challenge.<identifier>)` 分别构造。

#### R7 domain-alias 模式（`dns_alias` 非空白）
当 `dns_alias` 非空白时进入 domain-alias 模式：挑战记录写入名 MUST 直接取 `dns_alias` 的值（原样使用，不拼接 `_acme-challenge.` 前缀），表达 CNAME 形如 `_acme-challenge.A.com → myalias.B.com` 的委派形态。此模式下 `dns_zone` MUST 同时非空白——写入名所属 zone 无法从任意 `dns_alias` 可靠推断，必须显式给出；留空或纯空白时在任何 DNS 调用之前以可定位错误失败，错误信息含 `dns_alias` 值并提示 domain-alias 模式必须显式配置 `dns_zone`，不出现推导 zone 的调用。

- 写入名 MUST 属于 `dns_zone`（经 `relative_name` 校验），不属时以可定位错误失败，错误信息指出写入名 `<dns_alias>` 不属于 zone `<dns_zone>`。
- `dns_alias` 的值 MUST 不含 `_acme-challenge.` 前缀（否则与 challenge-alias 模式语义混淆）；填写带该前缀的值时 MUST 在任何 DNS 调用之前以可定位错误失败并提示改用 `dns_zone` 表达 challenge-alias 模式。空白（空字符串或纯空白）视为未填，走 challenge-alias / 同 zone 模式。
- CA 查询名恒为 `_acme-challenge.<授权域名去通配前缀>`，与 `dns_alias` / `dns_zone` 均无关。写入名与 CA 查询名不同时（domain-alias 与跨 zone challenge-alias 两种情形皆是）视为跨 zone CNAME 委派，传播等待与日志 MUST 使用 CA 查询名、DNS 提供商调用 MUST 使用写入名。
- domain-alias 模式下传播等待的解析器查询与权威 NS 直查均以 CA 查询名为目标并跟随 CNAME 链判定可见性，写入名可见不足以放行。

#### 实现索引

- `crates/acmecast-server/src/steps/apply.rs: zone_of` — 单标签剥离与验证
- `crates/acmecast-server/src/steps/apply.rs: resolve_wildcard_zone` — 多级通配的逐级探测，`probe` 以 `find_txt` 误码 `NoExist/找不到 zone/not exist/InvalidDomainName` 判定托管
- `crates/acmecast-server/src/steps/apply.rs: challenge_record` — 按 `dns_alias` 分流构造 `TxtRecord`（非空取原值、留空走 `challenge_write_name(zone)`），`TxtRecord::new` / `TxtRecord::delegated` 两条分支不变
- `crates/acmecast-server/src/steps/apply.rs: dns_alias_of` — `dns_alias` 纯函数校验（带挑战前缀拒绝、留空视为未填）
- `crates/acmecast-server/src/steps/apply.rs: complete_challenge` — `dns_alias` 非空白而 `dns_zone` 留空时的前置校验；zone 解析分支不动（domain-alias 模式下 `dns_zone` 必非空，走第一分支）
- `crates/acmecast-server/src/steps/provider_select.rs` — `dns_credential_id` 未指定时按注册表稳定序遍历 DNS 类凭据自动选择
- 前端校验：`cert.apply` 的 `dns_zone` 在多域或 `dns_alias` 非空白时必填（条件并集）

#### 用例对照表

| 输入 domains | 推导 dns_zone | 说明 |
|---|---|---|
| `*.gcli.cn` | `gcli.cn` | R1 |
| `*.x.idev.top` | `x.idev.top` 或其父域的已托管者 | R1 + 逐级探测 |
| `a.example.com` | `example.com` | R2 |
| `a.b.example.com` | `b.example.com` | R2 |
| `skiy.net` | `skiy.net` | R3 |
| `*.com` | 无法推导，必填 | R4 |
| `["example.com","*.example.com"]` | 必填 `dns_zone` | R4 |
| `a.com` + `dns_zone=b.com` + `dns_alias=myalias.b.com` | 显式 `b.com`、写入名 `myalias.b.com` | R7 |
| `dns_alias=myalias.other.com` + `dns_zone=b.com` | 报错（写入名不属于 zone） | R7 |
| `dns_alias=_acme-challenge.b.com` | 报错（带前缀，改用 `dns_zone`） | R7 |
| `dns_alias` 非空白 + `dns_zone` 留空 | 报错（domain-alias 必须显式 `dns_zone`） | R7 |

## cert.apply 表单字段顺序与布局

`cert.apply` 暴露给表单的输入 schema 的顶层对象 MUST 携带 `x-field-order`，按下列顺序列出全部顶层字段：`account_credential_id`、`challenge`、`contacts`、`dns_credential_id`、`dns_alias`、`domains`、`dns_zone`、`key_algorithm`、`wait_propagation`、`insecure_skip_verify`、`renew_before_days`，使前端按固定顺序渲染，对抗 `serde_json::Map` 按字母序重排 `properties`。

### 字段顺序

账号与挑战字段在前、DNS 字段紧随其后、其余字段按序收尾：

| 段 | 字段 |
|---|---|
| 账号 | `account_credential_id` |
| 挑战 | `challenge` |
| 联系人 | `contacts` |
| DNS | `dns_credential_id`、`dns_alias`、`domains`、`dns_zone` |
| 其余 | `key_algorithm`、`wait_propagation`、`insecure_skip_verify`、`renew_before_days` |

### `domains` 半宽布局

`domains` 字段（数组字段，默认整行渲染）MUST 在输入 schema 携带 `x-half-width: true`，使前端将其压成半宽，与 `dns_zone` 同行。DNS 相关字段两两同行：`dns_credential_id` 与 `dns_alias` 同行（各半宽），`domains`（半宽）与 `dns_zone` 同行，`contacts` 独占一行。

### `dns_alias` schema 声明

- `dns_alias` 字段 MUST 为可选 string，描述 MUST 说明 domain-alias 模式语义、值为不带 `_acme-challenge.` 前缀的 CNAME 目标 FQDN，且不得引用其他外部工具的参数名。
- `dns_zone` 的条件必填标记覆盖「`dns_alias` 非空白」（与「`domains` ≥ 2」取并集）；`dns_alias` 留空且仅 1 个域名时 `dns_zone` 不被要求必填。

### 实现索引

- `crates/acmecast-server/src/steps/apply.rs: AcmeApplyInput` — `dns_alias: Option<String>` 字段（`#[serde(default)]` + `#[schemars(...)]` 描述）
- `crates/acmecast-server/src/steps/apply.rs` — `cert.apply` 输入 schema 的顶层 `x-field-order` 与 `domains.x-half-width` 注入
- `frontend/src/components/SchemaForm.tsx: orderedEntries / SchemaForm` — `x-field-order` 定序与 `x-half-width` 渲染（既有能力，无需新增分支）

## DNS 提供商选择与 DoH 排序

`cert.apply` 的 DNS-01 流程以 `dns_credential_id` 为唯一凭据参数，不再接受 `dns_provider` 输入字段——提供商标识由凭据记录的 `type_id` 直接推导（如 `cloud.cloudflare` 类型凭据 → Cloudflare 提供商）。已有流水线配置中残留的 `dns_provider` 值在加载时被静默忽略，行为以凭据 `type_id` 为准。

### 显式指定凭据

`dns_credential_id` 非空时，系统从 `CredentialStore::resolve()` 返回的凭据 `type_id` 推导提供商标识，以此提供商完成 TXT 记录的写入、传播等待的解析器排序与记录清理。

### 自动选择（`dns_credential_id` 为空）

未指定凭据时，系统遍历凭据库中所有 DNS 类凭据（凭据 `type_id` 即候选标识），逐个探测其能否管理目标 zone，取第一个探测通过的（凭据，提供商）组合。候选遍历顺序为：按凭据 `type_id` 的注册表稳定序，同一类型多条凭据按 ID 升序。每个失败候选的原因经步骤日志输出；全部失败时步骤报错，错误中汇总各候选原因。

### DoH 解析器排序

传播等待阶段用于判定"CA 能否看到 TXT"的公共解析器集合按提供商标识感知排序：首选提供商对应的同厂商 DoH 端点排第一，其后按固定序排列其余厂商 DoH 作为兜底。排序规则：

| 提供商 | 首选 DoH | 兜底序 |
|---|---|---|
| `cloud.cloudflare` | Cloudflare | Cloudflare → 阿里 → 腾讯 → Google |
| `cloud.aliyun` | 阿里 | 阿里 → Cloudflare → 腾讯 → Google |
| `cloud.tencent` / `dns.tencent-eo` | 腾讯 | 腾讯 → Cloudflare → 阿里 → Google |
| 其他 / 未传参 | — | Cloudflare → 阿里 → 腾讯 → Google |

"任意一家可见即就绪"的判定语义不变。`ACMECAST_DOH_RESOLVERS` 显式设置时提供商感知分支不生效，按环境变量语义构造集合（`none` 即跳过解析器阶段）。

### 实现索引

- `crates/acmecast-server/src/steps/apply.rs: complete_challenge` — 从凭据 `type_id` 推导提供商标识
- `crates/acmecast-server/src/steps/provider_select.rs` — 注册表稳定序遍历自动选择
- `crates/acmecast-dns/src/propagation.rs: prioritize_for_provider` — 提供商感知 DoH 排序


## DNS 传播等待时序与轮次日志

`cert.apply` 的 DNS-01 传播等待由 `wait_until_visible_phased` 驱动，分四段等分调度，任一段命中即成功返回；`round x miss` 日志在入库前精简至仅保留最后一条，SSE 实时流保持全量。

### 启动两行日志

传播等待启动时产生两行 `info` 日志，顺序固定，均在 `round 1 miss` 之前：

1. `propagation start timeout=<s> interval=<s> (record: _acme-challenge.<domain>)` — 首行，内容不变。
2. `provider dns: <provider_id> (<resolver1>, <resolver2>, ...)` — 次行命中 DNS，命中集为整组 `ext_refs` 经 `prioritize_for_provider` 排序后的 `name()` 序列（`provider=cloudflare` 时整组 `Cloudflare DNS, 阿里 DNS, 腾讯 DNSPod, Google DNS`，首位为 Cloudflare），与后续 `round x miss (resolvers: ...)` 的 resolver 标识同源。

`provider` 未知/空时次行按固定兜底序 `Cloudflare → 阿里 → 腾讯 → Google`，但两行顺序不变。`propagation start` 本行不追加解析器上下文。

### 四阶段等分时序

`wait_until_visible_phased` 以 `timeout` 等分为四段，末段 DoH 再拆半：

| 阶段 | 时间窗 | 查询集 |
|---|---|---|
| `hit` | `[0, timeout/4)` | 整组 `ext_refs` provider 优先（首位为 `provider` 对应的 DoH） |
| `authoritative` | `[timeout/4, timeout/2)` | 权威 NS（`AuthoritativeResolver`），**不排除**已在 hit 出现的单家 |
| `all_dns` | `[timeout/2, 3*timeout/4)` | 整组 `ext_refs` |
| `doh` | `[3*timeout/4, timeout)` | 再拆半：`doh_single [3*timeout/4, 7*timeout/8)` 仅命中单家 → `doh_full [7*timeout/8, timeout)` 整组 ext_refs provider 优先 |

- 任一阶段命中即 `on_progress(hit)` 并成功返回，不等待后续阶段。
- `provider=cloudflare` 且 `ext_refs` 含四家时：`hit` 查整组 `Cloudflare DNS, 阿里 DNS, 腾讯 DNSPod, Google DNS`（首位 Cloudflare），`doh_single` 仅查 `Cloudflare DNS` 单家，`authoritative` 仍查权威 NS（不因已在 hit 出现而剔除），`doh_full` 查整组 `Cloudflare, 阿里, 腾讯, Google` 且首位为 Cloudflare。
- `ACMECAST_DOH_RESOLVERS=none`（或解析器切片为空）时三组均为空，走"提供商侧确认即就绪"短路。

### miss 日志入库精简（双轨语义）

SSE 实时推送与终态持久化遵循双轨：

- **SSE 实时流**（`RunExecutionSink::emit` → `HistoryCache::push_log` + `SseBroadcaster::broadcast_log`）：全量 `round x miss`，每轮 miss 产生一条 `log` 事件，含命中 DNS 行（整组），不因后续精简而提前过滤。
- **入库前精简**（`execute_run` 组装 `step_logs` 后、`HistoryRepository::finish` 前）：对 `message` 匹配 `round \d+ miss` 且含 `resolvers:` 的条目，仅保留最后一条（`rposition`），其余 `retain` 移除。命中 DNS 行（整组）、`round x hit`、`propagation start`、`propagation succeeded` 等非 miss 日志不受影响。

精简仅影响持久化与后续 `GET /api/histories/{id}/logs` / 缓存重放返回的持久化视图；SSE 已推送的实时流不受影响。空/单条/无 miss 时为 no-op。

### 实现索引

- `crates/acmecast-dns/src/propagation.rs: wait_until_visible_phased` — 四阶段等分调度 + 命中整组行 + 末段拆半
- `crates/acmecast-server/src/steps/apply.rs: complete_challenge` — 提供整组 `ext_refs`（provider 优先）给 phased 的 hit/doh_full 阶段
- `crates/acmecast-server/src/run.rs: execute_run` — `compact_propagation_miss_logs` 切入点（落库前压缩）
- `crates/acmecast-pipeline/src/runner.rs` — `mpsc → EventSink::emit(Log)` 增量分流（SSE 全量）
## DoH 解析器熔断剔除与代理自动回退

传播等待的 DoH 解析器集合在单次运行内支持连续网络失败熔断剔除，避免对确定不可达端点（如国内不可达的 `cloudflare-dns.com` / `dns.google`）的无效轮询（`#154` 实测每轮每端点 `LOOKUP_TIMEOUT=5s` 超时）。熔断仅裁剪本轮 DoH 查询子集，不改变"任一家可见即就绪"与四段等分超时语义。同时，DoH 解析器无凭据关联，无法通过凭据 `proxy_mode` 配置代理；当全局 `ACMECAST_PROXY_URL` 已配且 DoH 直连网络失败时，MUST 自动经代理回退重试，使国内不可达的 DoH 端点也能经代理完成传播确认。

### DoH 代理自动回退

- DoH 解析器的传输层初始为直连（`ReqwestTransport::new()`），不因全局 `ACMECAST_PROXY_URL` 已配而默认走代理——避免代理不可用导致全部 DoH 端点首轮即失败。
- DoH 端点直连查询发生"网络不可用"失败（见下节失败分类）且全局 `ACMECAST_PROXY_URL` 已配置时，MUST 自动用 `ReqwestTransport::with_proxy(global_proxy_url)` 构建代理传输层并重试该端点查询一次：
  - 重试成功（命中期望 TXT 或未命中 `Ok(_)`）→ 该端点视为可达，结果参与传播判定，并缓存该端点"需经代理"的传输偏好，后续轮次与后续阶段（`doh_single`/`doh_full`）直接用代理传输层查询，不再每轮重复直连探测。
  - 重试仍网络失败 → 计入该端点的连续失败计数（即直连失败 + 代理回退失败 = 2 次连续失败，下一轮再失败即第 3 次触发熔断剔除）。
  - 全局 `ACMECAST_PROXY_URL` 未配置 → 不做代理回退，直连失败直接计次。
- 传输偏好缓存（`HashMap<endpoint, ProxyPreference>`：`Direct` / `Proxy`）仅本轮传播会话内有效，下次运行 MUST 重置；`doh_single → doh_full` 切段时跨段保留。
- 代理回退仅影响 DoH 解析器，MUST NOT 改变权威 NS 直查与 `all_dns` 阶段的传输层；MUST NOT 改变凭据出网行为（凭据按自身 `proxy_mode` 独立构建传输层）。

### 触发与计数

- 同一 DoH 端点连续 3 次"网络不可用"失败 MUST 从本轮 `active_resolvers` 剔除，后续轮次不再查询该端点，仅查询剩余 `active`；阈值 3 写死，不做配置项。
- 每端点独立维护连续失败计数（`HashMap<endpoint, u8>`）与 `active: Vec<&dyn DnsResolver>` 子集，维护落在 `crates/acmecast-dns/src/propagation.rs` 的等待循环内（`wait_until_visible` 与 `wait_until_visible_phased` 的 `doh` 段），不下沉至 `HttpTransport` / `ReqwestTransport` 层；`probe_resolvers` 返回后按端点更新，`doh_single → doh_full` 切段时剔除状态跨段保留；Key 取端点归一化 endpoint 字符串，与 `resolvers.rs` 去重键一致。
- 成功（命中期望 TXT）或"未命中" MUST 清零该端点的连续失败计数，下一次网络失败重新从 1 开始计数，避免"2 次失败 + 1 次未命中"被误剔除。
- 熔断仅本轮传播会话内有效，下次流水线运行 MUST 重置，所有端点重新参与查询。

### 失败分类

- MUST 将连接超时（含 `LOOKUP_TIMEOUT=5s` 的 `tokio::time::timeout` 超时，已包装为 `Error::provider("…超时…")`）、连接拒绝、TLS 握手失败、代理不可达（全局 `ACMECAST_PROXY_URL` 指向的代理连不上）、DoH 端点域名解析失败等传输层 `Err` 归为"网络不可用"并计数；`reqwest::Error::is_connect` / `is_timeout` 与代理连接失败同口径，与凭据级代理的错误分类一致。
- MUST 将 HTTP 200 但无期望 TXT（含 `Ok([])` 空集或 `Ok(非期望值)`）视为"未命中"，MUST NOT 计入熔断、计数清零；`Ok` 一律不剔除可达端点。
- 代理回退重试的结果同样按上述分类：重试 `Ok` → 清零计数 + 缓存代理偏好；重试 `Err(网络不可用)` → 计入连续失败。

### 剔除与降级

- 计数达到 3 时 MUST `retain` 剔除并记 `warn`，含端点标识与连续失败次数。
- `active` 被剔空（全部 DoH 均被熔断）时 MUST 记 `warn "DoH全部不可用，跳过DoH验证"` 并跳过 DoH 阶段等待，不再轮询 DoH，直接按既有超时策略进入下一阶段或返回，避免卡死。
- 传播成功/超时的终态日志 MUST 附带剔除列表以便排障；`on_progress` / 日志中可观测到剔除端点。

### 作用域

- 仅影响 DoH 解析器：`wait_until_visible` 的 `resolvers` 全量 DoH 与 `wait_until_visible_phased` 的 `doh` 段；权威 NS 直查与 `all_dns` 阶段 MUST 保持全量探测，不受熔断裁剪、不受代理回退影响。
- `ACMECAST_DOH_RESOLVERS=none` 或解析器集合为空（短路成功）时本机制不生效。
- `probe_resolvers` 串行遍历 `active` 子集，剔除后遍历变短，无并发竞态；日志除剔除 `warn` 与终态列表外不额外刷屏。

### 实现索引

- `crates/acmecast-dns/src/propagation.rs: wait_until_visible` — DoH 全量熔断（计数/`active`/`probe_resolvers` 后更新/剔空降级）+ 代理回退（直连失败 → 读全局 `ACMECAST_PROXY_URL` → `with_proxy` 重试 → 缓存偏好）
- `crates/acmecast-dns/src/propagation.rs: wait_until_visible_phased` — 仅 `doh` 段熔断与代理回退，跨 `doh_single`/`doh_full` 保留状态与偏好
- `crates/acmecast-dns/src/propagation.rs: probe_resolvers` — 返回 `Ok`/`Err` 供分类（`Ok` 清零，`Err(网络不可用)` 计次/触发回退）
- `crates/acmecast-dns/src/resolvers.rs: doh_transport_from / doh_transport` — DoH 传输层构建（初始直连，代理回退时按全局 `ACMECAST_PROXY_URL` 构建）


## 内建 ACME CA（凭据 `acme.account` 的 `ca`）

| 别名 `ca` | 目录 URL（`CaKind::builtin_url`） | EAB | 有效期 | 通配 | SAN | 教程 / 申请入口 | 备注 |
|---|---|---|---|---|---|---|---|
| `letsencrypt` / `le` | https://acme-v02.api.letsencrypt.org/directory | 否 | 90 天 | 是 | 100 | https://github.com/acmesh-official/acme.sh/wiki/CA | — |
| `letsencrypt-staging` / `le-staging` | https://acme-staging-v02.api.letsencrypt.org/directory | 否 | — | 是 | 100 | 同上 | 测试环境 |
| `zerossl` | https://acme.zerossl.com/v2/DV90 | 是（凭据空时必填，创建后由 `credentials` 复用，下同） | 90 天 | 是 | 100 | https://app.zerossl.com/developer | — |
| `google` / `gts` | https://dv.acme-v02.api.pki.goog/directory | 是 | 90 天 | 是 | 100 | https://docs.cloud.google.com/certificate-manager/docs/public-ca-tutorial#request-key-hmac | 需 Google Cloud HMAC |
| `google-test` / `gts-test` | https://dv.acme-v02.test-api.pki.goog/directory | 是 | — | 是 | 100 | 同上（测试目录） | 测试目录 |
| `freessl` / `free-ssl` | https://acme.litessl.com/acme/v2/directory | 是 | 90 天 | 是 | — | https://freessl.cn/automation/eab-manager | 国内 FreeSSL |
| `custom`（示例 1: Actalis） | https://acme-api.actalis.com/acme/directory | 按目标 CA | — | — | — | https://guide.actalis.com/ssl/activation/acme | 需自行配置 `custom` |
| `custom`（示例 2: SSL.com） | https://acme.ssl.com/sslcom-dv-ecc 或 https://acme.ssl.com/sslcom-dv-rsa | 按订阅 | — | — | — | https://www.ssl.com/guide/ssl-tls-certificate-issuance-and-revocation-with-acme/ | `custom` 演示，原内置已移除，ECC/RSA 按 `key_algorithm` 二选一 |
| `custom`（通用） | （无内置，需显式 `directory_url`） | 按目标 CA | — | — | — | https://github.com/acmesh-official/acme.sh/wiki/CA | 任意 RFC8555 CA（需显式配置） |

- `resolve_directory_url(ca, custom_url)`: `custom_url` 非空以其为准，否则取 `builtin_url`；`custom` 且空时报错“自定义 ACME 必须填写 Directory URL”。
- 表单 `acme.account` 的 `ca` 字段：`x-enum-labels` 为上述中英文展示名，`x-help-links` 按值注入对应教程；`eab_kid`/`eab_hmac_key` 对 `freessl/zerossl/google/google-test` 在 `credentials` 为空时 `x-required-when: And(In[ca], Empty[credentials])`，有有效 `credentials`（JSON）时豁免复用。

## 凭据能力标记（`capabilities`）

同一云平台（腾讯云 / Cloudflare / 阿里云）的密钥权限可按使用场景拆分为 DNS 与部署两类。凭据记录新增 `capabilities` 字段，用于声明该凭据可用于 `dns`（DNS-01 挑战）与 `deploy`（证书部署），贯穿存储、API、表单、下拉过滤与执行校验。

### 受控类型

仅 `cloud.tencent`、`cloud.cloudflare`、`cloud.aliyun` 三类凭据支持能力标记；其他类型（`dns.tencent-eo`、`deploy.ssh`、`deploy.s3`、`deploy.webdav`、`acme.account` 等）不暴露、不持久化、不返回该字段。

| 能力值 | 用途 |
|---|---|
| `dns` | 可用于 `cert.apply` 的 DNS-01 挑战（TXT 记录写入与清理） |
| `deploy` | 可用于 `cert.deploy` 的证书部署（如腾讯云 SSL） |

### 存储与归一化

- `acmecast_credential.capabilities` 以 JSON 数组文本存储，默认值 `["dns","deploy"]`。
- 归一化：写入前白名单过滤、去重、排序，保证 `["deploy","dns"]` 与 `["dns","deploy"]` 库中形式一致。
- 缺省 / 空 / 非法 JSON 视为双能力 `["dns","deploy"]`，保证存量受控凭据在新过滤逻辑下不被误隐藏。

### API 透传与校验

- `CredentialRequest` 携带 `capabilities: Option<Vec<String>>`，`CredentialResponse` / `CredentialDetailResponse` 返回 `capabilities: Vec<String>`（非受控类型返回空）。
- 受控类型：`None` 视为未传（服务端补默认双能力）；空数组或含非法值返回 400 且 `field=capabilities`；至少选一项。
- 非受控类型：忽略输入，不持久化、不返回。

### 凭据表单

- 凭据编辑弹窗在 `type_id` 为受控类型时展示「可用权限」多选（`Checkbox.Group`，选项 `DNS` / `部署`），新建默认双选，编辑回填现值，切换类型时重置或隐藏。
- 能力字段为外层表单项（与 `name` / `remark` 同级），独占一行，不进 `SchemaForm`，不与其他字段并排。

### 凭据列表

- 列表新增「可用权限」列：受控类型展示 `DNS` / `部署` 标签（双选并列），非受控类型显示 `—`。
- 该列保持与「名称」「类型」「备注」等列一致的默认左对齐，不得设置为居中对齐（`align: "center"`）。

### 流水线下拉过滤

流水线步骤编辑表单中凭据下拉按能力过滤，过滤依据为凭据列表 API 返回的 `capabilities`：

| 字段 | 过滤条件 |
|---|---|
| `dns_credential_id`（`cert.apply`） | 仅列 `capabilities` 含 `dns` 的凭据 |
| `credential_id`（`cert.deploy` 的 `tencent-ssl` 变体） | 仅列 `type_id == "cloud.tencent"` 且 `capabilities` 含 `deploy` 的凭据 |

缺省（空 / 非受控类型）视为双能力，不因能力被过滤。过滤后下拉仍保留名称 + `id` + 备注的可读性约定。

### 流水线保存与执行校验

- **保存校验**：流水线保存时，`cert.apply` 的 `dns_credential_id` 与 `cert.deploy` 的 `tencent-ssl credential_id` 引用凭据的能力不含所需用途时，返回 400 并提示凭据 ID、名称与缺失能力，不发起云端调用。
- **执行校验**：部署执行时（如 `TencentSslTarget::resolve`），在解密字段之前、任何云端调用之前校验 `deploy` 能力，缺失时以可读错误失败。

### 连通性测试

凭据连通性测试仅校验凭据在对应云平台的身份有效性，不因 `capabilities` 取值而改变探测目标或判定结果。能力仅影响流水线侧的「能否被选 / 能否执行」。

### 实现索引

- `crates/acmecast-store/src/capability.rs` — 能力枚举、受控类型白名单、归一化与解析（`parse` / `normalize` / `to_text` / `is_controlled`）
- `crates/acmecast-store/src/entity/credential.rs` — `capabilities` 列与 `ActiveModel`
- `crates/acmecast-access/src/credential_store.rs: ResolvedCredential` — 解析后凭据携带 `capabilities`
- `crates/acmecast-server/src/handlers.rs: resolve_capabilities_text` — API 层校验（空数组 / 非法值 400）
- `crates/acmecast-server/src/handlers.rs: validate_pipeline_credential_capabilities` — 流水线保存时能力校验
- `crates/acmecast-deploy/src/targets/tencent.rs: resolve_tencent_credentials` — 部署执行时 `deploy` 能力校验
- `frontend/src/pages/credentials/CredentialListPage.tsx` — 凭据表单多选与列表标签列
- `frontend/src/pages/pipelines/PipelineEditorPage.tsx: hasCapability` — 流水线下拉能力过滤


## 凭据类型展示名（`display_name`）

凭据类型的展示名由后端 `CredentialType::display_name()` 提供，`GET /api/credential-types` 返回 `display_name`，前端新建/编辑弹窗的类型下拉与凭据列表均直接使用该值，不在前端做字符串裁剪。展示名不再用中文括号 `（…）` 追加使用范围标注——能力多选与列表能力标签已表达用途。

| `type_id` | 展示名 |
|---|---|
| `cloud.tencent` | 腾讯云 |
| `dns.tencent-eo` | 腾讯云 EdgeOne |
| `cloud.cloudflare` | Cloudflare |
| `cloud.aliyun` | 阿里云 |
| `deploy.ssh` | SSH 主机 |
| `deploy.s3` | S3 兼容存储 |
| `deploy.webdav` | WebDAV |
| `acme.account` | ACME 账号 |

`type_id` 采用带命名空间的点分命名（见「凭据 type_id 命名空间」节），展示名不受标识变更影响。依赖 `display_name` 做精确匹配的外部脚本需更新。

### 实现索引

- `crates/acmecast-server/src/credentials.rs` — `display_name()` 返回短名称
- `crates/acmecast-server/src/handlers.rs: list_credential_types` — 透传 `display_name`
- `frontend/src/pages/credentials/CredentialListPage.tsx` — 类型下拉 `options[].label = display_name`

## 凭据 type_id 命名空间

凭据 `type_id` 采用带命名空间的点分命名，使标识本身即表达用途归属。DNS 提供商的 `type_id` 与凭据 `type_id` 1:1 对齐（`cert.apply` 靠 `credential.type_id == dns_provider.type_id` 自动选择），两边同步使用新标识。

| 前缀 | 语义 | 类型 |
|---|---|---|
| `acme.` | ACME 账号 | `acme.account` |
| `cloud.` | 通用云平台（DNS + 部署双能力） | `cloud.tencent`、`cloud.aliyun`、`cloud.cloudflare` |
| `dns.` | 仅 DNS | `dns.tencent-eo` |
| `deploy.` | 仅部署 | `deploy.ssh`、`deploy.s3`、`deploy.webdav` |

`cloud.*` 三类同时是能力标记的受控类型（`capabilities`）。部署目标标识（`s3`/`webdav`/`tencent-ssl`/`ssh`/`local`）作为部署目标注册表的 key 不受此变更影响，只改它们接受的**凭据** `type_id` 白名单（`ACCEPTED_CREDENTIAL_TYPES` 与 schema 的 `x-credential-types` 注入值）。

### 旧→新映射

| 旧 type_id | 新 type_id |
|---|---|
| `cloudflare` | `cloud.cloudflare` |
| `aliyun` | `cloud.aliyun` |
| `tencent` | `cloud.tencent` |
| `tencent-eo` | `dns.tencent-eo` |
| `ssh` | `deploy.ssh` |
| `s3` | `deploy.s3` |
| `webdav` | `deploy.webdav` |
| `acme.account` | `acme.account`（不变） |

### 凭据白名单同步

- `CONTROLLED_TYPES`：`["cloud.tencent", "cloud.cloudflare", "cloud.aliyun"]`
- 部署目标 `ACCEPTED_CREDENTIAL_TYPES`：`tencent-ssl → ["cloud.tencent"]`、`s3 → ["deploy.s3"]`、`webdav → ["deploy.webdav"]`
- 前端 `DNS_CREDENTIAL_TYPE_IDS`：`cloud.cloudflare`、`cloud.aliyun`、`cloud.tencent`、`dns.tencent-eo`
- 前端 `CAPABILITY_CONTROLLED_TYPES`：同 `CONTROLLED_TYPES`
- 前端 `SSH_HOST_TYPE_ID`：`"deploy.ssh"`，`ACME_ACCOUNT_TYPE_ID`：`"acme.account"`（不变）

### 存量凭据 type_id 自动迁移

系统升级时通过数据库迁移将 `acmecast_credential.type_id` 的旧值重写为新值（上表 7 条映射，`acme.account` 不动）。迁移幂等：重复执行不再改已是新值或不命中旧值的行；对空库无影响。失败时由现有 `migrate_with` 机制中止启动并指明失败版本。

### 实现索引

- `crates/acmecast-server/src/credentials.rs` — 凭据 `type_id()` 返回新标识
- `crates/acmecast-dns/src/providers/{cloudflare,aliyun,tencent,tencent_eo}.rs` — DNS 提供商 `type_id()` 同步
- `crates/acmecast-dns/src/propagation.rs` — DoH resolver 感知的 type_id 字符串同步
- `crates/acmecast-store/src/capability.rs: CONTROLLED_TYPES` — 受控类型白名单
- `crates/acmecast-deploy/src/targets/{s3,webdav,tencent}.rs` — `ACCEPTED_CREDENTIAL_TYPES` 与 `x-credential-types` 注入
- `crates/acmecast-deploy/src/ssh_host.rs: SSH_HOST_TYPE_ID` — SSH 主机档案标识
- `crates/acmecast-access/src/credential/{s3,webdav}.rs` — `TYPE_ID` 常量
- `crates/acmecast-store/src/migration/` — 存量 type_id 迁移文件（幂等）
- `frontend/src/pages/credentials/CredentialListPage.tsx` — `CAPABILITY_CONTROLLED_TYPES`
- `frontend/src/pages/pipelines/PipelineEditorPage.tsx` — `DNS_CREDENTIAL_TYPE_IDS`、`SSH_HOST_TYPE_ID`、`CAPABILITY_CONTROLLED_TYPES`

## 凭据字段表单布局约定

凭据字段表单由后端 schema（`schemars::JsonSchema` + `x-*` 扩展）驱动，前端 `SchemaForm` 为通用渲染引擎。默认标量字段半宽（`span 12`）两列一行；带 `x-full-width` 的字段独占整行；带 `x-multiline` 的字段渲染为 textarea 并独占整行；带 `x-end-row` 的半宽字段行尾补空占位列换行；`x-field-order` 钉字段渲染顺序，对抗 `serde_json::Map` 按字母序重排 `properties`；`proxy_url` 统一带 `x-visible-when: Equals(field=proxy_mode, values=["custom"])` 仅在 `proxy_mode=custom` 时显隐；`proxy_mode` 统一注入 `enum ["direct","inherit","custom"]` 与 `x-enum-labels {直连,继承全局,自定义}`。代理行（`proxy_mode` 左下拉｜`proxy_url` 右输入框）是否需前驱 `x-end-row` 由 `acmecast_core::inject_proxy_field_annotations` 按 24 栅格流式模拟自动判定：仅当前驱处于左列（`col==12`）时补空列把代理推到新行，处于行尾或整行后自然换行则不补。

### 表单页排序和布局（凭据页新建/编辑）

以下 12 类凭据的字段顺序、分组与宽度标注为唯一真实来源，前端按此渲染，不在渲染器内写类型分支。`仅一行`均指独占一行（`x-full-width` 整行或半宽 `x-end-row` 右侧空占位，见宽度标注）。

#### `acme.account` — ACME 账号

`x-field-order: ["ca","directory_url","eab_kid","eab_hmac_key","credentials","proxy_mode","proxy_url"]`

| 行 | 字段 | 宽度标注 |
|---|---|---|
| 1 | `ca` | 仅一行：半宽 + `x-end-row`（下拉半列独占一行，右侧空占位承载 `x-help-links`） |
| 2 | `directory_url` | 仅一行：`x-full-width` 整行（长 URL） + `x-required-when: ca=custom` |
| 3 | `eab_kid` ｜ `eab_hmac_key` | 半宽同行 |
| 4 | `credentials` | 一行 `x-multiline` textarea 整行 |
| 5 | `proxy_mode` ｜ `proxy_url` | 半宽同行，`proxy_url` 联动显隐；`credentials` 为整行，前驱无需 `x-end-row` |

#### `deploy.webdav` — WebDAV

`x-field-order: ["url","base_path","username","password","proxy_mode","proxy_url"]`

| 行 | 字段 | 宽度标注 |
|---|---|---|
| 1 | `url` ｜ `base_path` | 半宽同行 |
| 2 | `username` ｜ `password` | 半宽同行 |
| 3 | `proxy_mode` ｜ `proxy_url` | 半宽同行；`password` 已在行尾自然换行，无需 `x-end-row` |

#### `deploy.s3` — S3 兼容存储

`x-field-order: ["access_key","secret_key","endpoint","bucket","region","prefix","session_token","proxy_mode","proxy_url"]`

| 行 | 字段 | 宽度标注 |
|---|---|---|
| 1 | `access_key` ｜ `secret_key` | 半宽同行（密钥对） |
| 2 | `endpoint` ｜ `bucket` | 半宽同行（接入与桶） |
| 3 | `region` ｜ `prefix` | 半宽同行（区域与前缀） |
| 4 | `session_token` | 仅一行：半宽 + `x-end-row`（由代理注入自动补列，独占一行左侧，右侧空占位） |
| 5 | `proxy_mode` ｜ `proxy_url` | 半宽同行 |

#### `deploy.ssh` — SSH 主机

`x-field-order: ["cert_mode","key_mode","host","port","user","password","private_key","proxy_mode","proxy_url"]`

| 行 | 字段 | 宽度标注 |
|---|---|---|
| 1 | `cert_mode` ｜ `key_mode` | 半宽同行（文件权限缺省 `0644`/`0600`） |
| 2 | `host` ｜ `port` | 半宽同行 |
| 3 | `user` ｜ `password` | 半宽同行 |
| 4 | `private_key` | 一行 `x-multiline` textarea 整行 |
| 5 | `proxy_mode` ｜ `proxy_url` | 半宽同行；`private_key` 为整行，前驱无需 `x-end-row` |

#### `cloud.tencent` — 腾讯云

`x-field-order: ["account_site","secret_id","secret_key","proxy_mode","proxy_url"]`（与 `dns.tencent-eo` 共用 `tencent_credentials_schema`）

| 行 | 字段 | 宽度标注 |
|---|---|---|
| 1 | `account_site` | 仅一行：半宽 + `x-end-row`（站点选择独占一行左侧，右侧空占位） |
| 2 | `secret_id` ｜ `secret_key` | 半宽同行 |
| 3 | `proxy_mode` ｜ `proxy_url` | 半宽同行；`secret_id`｜`secret_key` 已占满一行，自然换行无需 `secret_key` 补 `x-end-row` |

#### `cloud.cloudflare` — Cloudflare

`x-field-order: ["api_token","proxy_mode","proxy_url"]`

| 行 | 字段 | 宽度标注 |
|---|---|---|
| 1 | `api_token` | 仅一行：半宽 + `x-end-row`（Token 独占一行左侧，右侧空占位） |
| 2 | `proxy_mode` ｜ `proxy_url` | 半宽同行 |

#### `cloud.aliyun` — 阿里云

`x-field-order: ["access_key_id","access_key_secret","proxy_mode","proxy_url"]`

| 行 | 字段 | 宽度标注 |
|---|---|---|
| 1 | `access_key_id` ｜ `access_key_secret` | 半宽同行 |
| 2 | `proxy_mode` ｜ `proxy_url` | 半宽同行；密钥对已占满一行，自然换行无需 `access_key_secret` 补 `x-end-row` |

#### `dns.tencent-eo` — 腾讯云 EdgeOne

与 `cloud.tencent` 完全一致，共用 `tencent_credentials_schema`：`account_site` 仅一行（半宽 + `x-end-row` 独占一行）｜ `secret_id`｜`secret_key` ｜ `proxy_mode`｜`proxy_url`。

#### `notify.generic` — 通用 Webhook

`x-field-order: ["url","method","headers","body_template","proxy_mode","proxy_url"]`

| 行 | 字段 | 宽度标注 |
|---|---|---|
| 1 | `url` | 仅一行：`x-full-width` 整行 |
| 2 | `method` | 仅一行：半宽 + `x-end-row`（枚举下拉半列独占一行，右侧空占位） |
| 3 | `headers` | 仅一行：`x-multiline` textarea 整行 |
| 4 | `body_template` | 仅一行：`x-multiline` textarea 整行 |
| 5 | `proxy_mode` ｜ `proxy_url` | 半宽同行，`proxy_url` 联动显隐；`body_template` 为整行，前驱无需 `x-end-row` |

#### `notify.email` — 邮件

`x-field-order: ["host","port","from","tls","to","username","password","proxy_mode","proxy_url"]`

| 行 | 字段 | 宽度标注 |
|---|---|---|
| 1 | `host` ｜ `port` | 半宽同行 |
| 2 | `from` ｜ `tls` | 半宽同行，`tls` 为枚举下拉（`implicit`/`starttls`/`none`） |
| 3 | `to` | 仅一行：`x-multiline` + `x-full-width` textarea 整行（每行一个收件人） |
| 4 | `username` ｜ `password` | 半宽同行 |
| 5 | `proxy_mode` ｜ `proxy_url` | 半宽同行，`proxy_url` 联动显隐；`password` 已在行尾自然换行，无需 `x-end-row` |

#### `notify.feishu` — 飞书机器人

`x-field-order: ["url","secret","proxy_mode","proxy_url"]`

| 行 | 字段 | 宽度标注 |
|---|---|---|
| 1 | `url` | 仅一行：`x-full-width` 整行 |
| 2 | `secret` | 仅一行：半宽 + `x-end-row`（可选签名密钥，独占一行左侧，右侧空占位） |
| 3 | `proxy_mode` ｜ `proxy_url` | 半宽同行，`proxy_url` 联动显隐 |

#### `notify.dingtalk` — 钉钉机器人

与 `notify.feishu` 完全一致：`x-field-order: ["url","secret","proxy_mode","proxy_url"]`，`url` 整行 ｜ `secret` 半宽独占一行 ｜ `proxy_mode`｜`proxy_url`。

### 实现索引

- `crates/acmecast-access/src/credential/acme_account.rs: AcmeAccountType::fields_schema` — `x-field-order` + `ca.x-end-row` + `directory_url.x-full-width` + `credentials.x-multiline`
- `crates/acmecast-access/src/credential/webdav.rs: webdav_fields_schema` — `x-field-order: ["url","base_path","username","password","proxy_mode","proxy_url"]`
- `crates/acmecast-access/src/credential/s3.rs: s3_fields_schema` — `x-field-order` 8 字段 + `session_token` 由代理注入补 `x-end-row`
- `crates/acmecast-deploy/src/ssh_host.rs: ssh_host_fields_schema` — `x-field-order` 9 字段 + `private_key.x-multiline`
- `crates/acmecast-notify/src/notify_credential.rs: {feishu,dingtalk,generic,email}_fields_schema` — `notify.*` 四类版式（见上）+ `proxy_mode/proxy_url` 注入
- `crates/acmecast-dns/src/providers/tencent.rs: tencent_credentials_schema` — `account_site.x-end-row` 先于代理注入，`secret_id`｜`secret_key` 同行
- `crates/acmecast-dns/src/providers/cloudflare.rs: cloudflare_credentials_schema` — `api_token.x-end-row`
- `crates/acmecast-dns/src/providers/aliyun.rs: aliyun_credentials_schema` — 密钥对同行，代理自然换行
- `crates/acmecast-dns/src/providers/tencent_eo.rs` — 复用 `tencent_credentials_schema`
- `crates/acmecast-core/src/proxy.rs: inject_proxy_field_annotations` — 代理下拉/联动/中文标签与 `x-end-row` 栅格判定单一来源
- `frontend/src/components/SchemaForm.tsx: orderedEntries / SchemaForm / ObjectFieldGroup` — `x-field-order` 定序与 `x-*` 渲染
## 凭据出网代理与全局代理配置

**全部凭据类型**（`cloud.tencent` / `dns.tencent-eo` / `cloud.cloudflare` / `cloud.aliyun` / `deploy.ssh` / `deploy.s3` / `deploy.webdav` / `acme.account` / `notify.feishu` / `notify.dingtalk` / `notify.generic` / `notify.email`）支持凭据级代理模式（`proxy_mode` 下拉 + `proxy_url` 条件输入），全局代理经 `config.toml` 的 `[proxy]` 段或环境变量 `ACMECAST_PROXY_URL` 设一次，供凭据 `inherit` 继承（DNS、部署与通知链路同口径）与 DoH 代理回退共用。存量凭据缺字段视为 `direct`（直连），无需数据库迁移。

### 全局代理（`ACMECAST_PROXY_URL` / `[proxy]` 段）

- 全局代理 URL 支持三种配置方式，合并优先级与项目一致为 **`builtin < config.toml < env`**：

| 优先级 | 来源 | 键 | 说明 |
|---|---|---|---|
| 1（内置） | — | — | 未配置时直连 |
| 2（文件） | `config.toml` | `[proxy]` 段 `url` | 可选 `String`，留空 / 缺段即直连 |
| 3（env） | 环境变量 | `ACMECAST_PROXY_URL` | 覆盖文件值 |

- `config.toml` 新增 `[proxy]` 段（`deny_unknown_fields`）：

+```toml
+[proxy]
+# 全局出网代理：凭据 proxy_mode=inherit 继承此值，DoH 解析器代理回退也读此值。
+# 支持 socks5://、http://、https://；留空或省略此段即直连。
+# 环境变量 ACMECAST_PROXY_URL 可覆盖此值。
+url = "socks5://127.0.0.1:7890"
+```

- `ServerConfig` 新增 `proxy_url: Option<String>` 字段，`apply_file` 从 `[proxy]` 段读取、`apply_env` 从 `ACMECAST_PROXY_URL` 覆盖，传递给 `doh_transport()` 与 `transport_for_credentials` 的 `inherit` 分支。
- 取值 MUST 经 `validate_proxy_url` 校验（仅 `socks5://` / `http://` / `https://` + 可解析）；env 非法值在启动时被拒绝并提示支持的 scheme，文件非法值在 `apply_file` 校验时拒绝启动；未配置 / 空串 / 仅空白 MUST 维持直连（现状行为）。
- 合并废弃 `ACMECAST_DOH_PROXY_URL`：原 DoH 专用代理环境变量不再独立存在，DoH 与凭据共用同一全局代理（`[proxy] url` / `ACMECAST_PROXY_URL`）。
- 标准 `HTTPS_PROXY` 等环境变量 MUST NOT 被隐式读取而波及未配代理的凭据（凭据级 `proxy_mode=direct` 保持显式直连）。

### 网络操作超时（`[network]` 段 / `ACMECAST_NETWORK_TIMEOUT_SECS`）

流水线各步骤的网络操作（SSH 连接/传输、S3 PUT、WebDAV PUT/MKCOL、腾讯云 API 调用、DNS 提供商 API 调用）MUST 受可配置超时约束，默认 30 秒，超时以可读错误返回而非永久挂起。已有独立超时的路径不受影响：ACME 轮询（120 秒）、DNS 传播等待（默认 300 秒）、通知投递（10 秒）、凭据连通性测试（10 秒）。

| 优先级 | 来源 | 键 | 说明 |
|---|---|---|---|
| 1（内置） | — | — | 30 秒 |
| 2（文件） | `config.toml` | `[network]` 段 `timeout_secs` | 正整数秒 |
| 3（env） | 环境变量 | `ACMECAST_NETWORK_TIMEOUT_SECS` | 覆盖文件值 |

- `config.toml` 新增 `[network]` 段（`deny_unknown_fields`）：

```toml
[network]
# 流水线网络操作超时（秒）：SSH 连接/传输、S3/WebDAV 上传、腾讯云 API、DNS API。
# 默认 30；环境变量 ACMECAST_NETWORK_TIMEOUT_SECS 可覆盖此值。
timeout_secs = 30
```

- `ServerConfig` 新增 `network_timeout: Duration` 字段，`apply_file` 从 `[network]` 段读取、`apply_env` 从 `ACMECAST_NETWORK_TIMEOUT_SECS` 覆盖，传递给 `default_dns_registry` 与 `default_deploy_registry`，再透传到各 DNS 提供商（`with_global_proxy_and_timeout`）与部署目标（`SshTarget::live` / `S3Target::new` / `WebdavTarget::new` / `TencentSslTarget::new`）。`acmecast_core::NETWORK_TIMEOUT`（30 秒）保留为编译期兜底默认值，运行时被配置值覆盖。
- 值为 0、负数或非数字时 MUST 在启动时拒绝并提示期望正整数（比 `[propagation]` 段的静默忽略更严）。
- `ReqwestTransport::new(timeout)` / `with_proxy(url, timeout)` 在 reqwest 客户端构建链上加 `.timeout(timeout)`，覆盖 DNS 提供商 API 调用与腾讯云 SSL API 调用。WebDAV `webdav_put` 的 reqwest 客户端同样加 `.timeout()`。S3 `s3_put` 用 `tokio::time::timeout` 包裹 AWS SDK 的 `put_object().send()`。SSH 的 `connect_ssh`/`open_proxy_stream`/`run_channel` 各用 `tokio::time::timeout` 包裹。
- 超时从代理隧道建立后开始计算：当凭据生效代理 URL 非空时（`custom` 自带或 `inherit` 全局非空），超时计时从代理隧道 TCP 流建立完成后开始（代理 TCP 连接阶段有操作系统级 TCP 超时兜底，不计入）。直连时从连接发起后开始计时。

### 凭据级代理模式（`proxy_mode` + `proxy_url`）

- **全部凭据类型**（`cloud.cloudflare` / `cloud.aliyun` / `cloud.tencent` / `dns.tencent-eo` / `deploy.ssh` / `deploy.s3` / `deploy.webdav` / `acme.account`）暴露 `proxy_mode` 与 `proxy_url` 字段。
- `proxy_mode` 三态语义：

| `proxy_mode` | `proxy_url` | 行为 |
|---|---|---|
| `direct`（默认） | 隐藏 | 直连，不继承全局代理 |
| `inherit` | 隐藏 | 继承全局 `ACMECAST_PROXY_URL`（未配则直连） |
| `custom` | 显示输入框 | 走该 URL，覆盖全局代理 |

- `custom` 时 MUST 校验 scheme ∈ `{socks5, http, https}` 且整体可解析；其他 scheme（如 `ftp://`）或无法解析的值（如 `not-a-url`）MUST 在凭据保存时被拒绝，错误定位到 `proxy_url` 字段并提示支持的三种 scheme。
- 校验口径单一来源：`validate_proxy_url`（凭据保存校验与带代理客户端构建共用）。
- 前端渲染标注统一由 `inject_proxy_field_annotations(&mut RootSchema)` 注入（全部凭据类型共用，不会出现"实现改了、标注丢了"漂移）：
  - `proxy_mode`：`enum_values: ["direct","inherit","custom"]` + `x-enum-labels: {direct:"直连", inherit:"继承全局", custom:"自定义"}`。schemars 对 `Option<String>` 不产 `enum` 数组，MUST 手动注入 `enum_values`，否则前端 `SchemaForm` 的 `node.enum.length > 0` 分支不命中、退化为普通文本输入框。
  - `proxy_url`：`x-visible-when: Equals(field=proxy_mode, value=custom)` 联动显隐。
  - `proxy_mode` 前一个字段（由顶层 `x-field-order` 确定）：`x-end-row: true`——使其半宽行尾补空占位列，`proxy_mode` 从新行首列开始，与 `proxy_url` 自然成对同行（左下拉、右输入框；`custom` 时右侧显示输入框，其他模式右侧空位）。
- 凭据 schema 的展示字段顺序 MUST 经顶层 `x-field-order` 钉住，代理模式与 URL 置于既有凭据字段之后，前端由 schema 驱动自动渲染，不新增渲染器分支：

| 凭据类型 | `x-field-order` 末尾追加 |
|---|---|
| `cloud.tencent` / `dns.tencent-eo` | `["account_site","secret_id","secret_key","proxy_mode","proxy_url"]` |
| `cloud.cloudflare` | `["api_token","proxy_mode","proxy_url"]` |
| `cloud.aliyun` | `["access_key_id","access_key_secret","proxy_mode","proxy_url"]` |
| `deploy.ssh` | `[…既有字段…,"proxy_mode","proxy_url"]` |
| `deploy.s3` | `["access_key","secret_key","endpoint","bucket","region","prefix","session_token","proxy_mode","proxy_url"]` |
| `deploy.webdav` | `[…既有字段…,"proxy_mode","proxy_url"]` |
| `acme.account` | `[…既有字段…,"proxy_mode","proxy_url"]` |

- 存量凭据兼容：旧 `proxy_url` 有值 → 视为 `proxy_mode=custom` + 该值；无值 / 缺字段 → `proxy_mode=direct`（直连）。无数据库迁移。

### 出网路径与连通性测试

- 各凭据类型的出网按 `proxy_mode` 选传输层（`direct` 直连 / `inherit` 读 `ServerConfig.proxy_url` / `custom` 读凭据 `proxy_url`）。`inherit` 对 DNS 与部署链路均继承全局代理，此前部署链路 `inherit` 等同直连的限制已移除：

| 凭据类型 | 出网路径 | 代理接入方式 |
|---|---|---|
| DNS 4 类 | DNS-01 挑战查询/创建/删除 + 连通性测试 | `ReqwestTransport::with_proxy`（`reqwest::Proxy::all`，socks5/http/https） |
| `deploy.webdav` | MKCOL/PUT 探测与部署上传 | reqwest `Proxy::all` |
| `deploy.s3` | `PutObject`/`DeleteObject` 探测与部署上传 | `aws_smithy_http_client::proxy::ProxyConfig`（http/https；socks5 用 reqwest 包装自定义 `HttpClient`） |
| `acme.account` | ACME 目录/下单/校验 | `acmecast_acme::ProxyConfig`（socks5/http/https） |
| `deploy.ssh` | SSH 连接与主机探测 | SOCKS5/HTTP-CONNECT 隧道 + `russh::client::connect_stream` |
| `deploy.tencent-ssl` | 腾讯云 SSL API 调用 | `ReqwestTransport`（经 `transport_for_credentials` 按 `proxy_mode` 选层） |
| `notify.feishu` / `notify.dingtalk` / `notify.generic` | 运行结束简讯投递 + 连通性测试 | `reqwest::Proxy::all`（socks5/http/https，`acmecast_notify::delivery::http_client_with_proxy`） |
| `notify.email` | 运行结束简讯投递 + 连通性测试 | SOCKS5/HTTP-CONNECT 隧道 + `lettre::AsyncSmtpConnection::connect_with_transport`（隐式 TLS 经 `tokio-rustls`） |

- 配置了代理（`inherit` 全局非空 或 `custom`）的凭据，其对应出网 MUST 经该代理发送；代理连接失败 MUST 以可读错误呈现（HTTP 类为 `provider_api_error`，SSH 类为连接错误）且含代理主机以便排障，含凭据的代理 URL 在日志与错误呈现前经 `redact` 脱敏（`socks5://user:pass@host:port` 不明文外泄）。
- 各类型的连通性测试（`CredentialType::test_connectivity`）MUST 走与正式调用相同的（按 `proxy_mode` 构建的）代理；错误同样经 `redact` 脱敏后呈现。
- 代理仅作用于**持有该凭据的调用**，MUST NOT 改变其他凭据或未走代理的通知渠道的出网路径；测试替身 `with_transport` 不受影响。
- `ReqwestTransport::with_proxy(proxy_url)` 内部以 `reqwest::Proxy::all(url)` 构建（`socks` feature 已启用），`new()`/`default()` 保持直连语义不变。

### 实现索引

- `crates/acmecast-core/src/proxy.rs: validate_proxy_url / ProxyMode / proxy_url_from_credentials / effective_proxy_url / inject_proxy_field_annotations` — 代理工具下沉共享（`acmecast-access` 不依赖 `acmecast-dns`）；`acmecast-dns` 保留 re-export；`effective_proxy_url` 为 DNS 与部署链路共用的 `inherit` 判定单一来源
- `crates/acmecast-dns/src/http.rs: ReqwestTransport::with_proxy / new / default` — 直连与带代理 Client 构建；`new(timeout)` / `with_proxy(url, timeout)` 接收网络超时参数
- `crates/acmecast-dns/src/http.rs: transport_for_credentials` — 按 `proxy_mode` 选传输层（DNS 与部署链路共用）
- `crates/acmecast-server/src/config.rs: ServerConfig.proxy_url / ProxySection / NetworkSection / apply_file / apply_env / template_content` — 全局代理字段、`[proxy]` 段、`[network]` 段反序列化与模板
- `crates/acmecast-dns/src/providers/{cloudflare,tencent,aliyun,tencent_eo}.rs: http_for` — DNS 提供商凭据代理接线（`global_proxy_url` 成员 + `with_global_proxy_and_timeout`）
- `crates/acmecast-access/src/credential/{s3,webdav,acme_account}.rs: {s3,webdav}_fields_schema / fields_schema` — 非 DNS 凭据 schema 注入代理字段
- `crates/acmecast-deploy/src/ssh_host.rs: ssh_host_fields_schema` — SSH 主机档案 schema 注入代理字段
- `crates/acmecast-deploy/src/targets/ssh.rs: SshTarget::live(proxy_url, timeout)` — SSH 部署持有 `global_proxy_url`，`resolve_input` 与 `probe_host` 按 `effective_proxy_url(profile, global_proxy_url)` 选代理
- `crates/acmecast-deploy/src/targets/s3.rs: S3Target::new(proxy_url, timeout)` — S3 部署持有 `global_proxy_url`，`s3_put` 按 `effective_proxy_url(fields, global_proxy_url)` 选 `s3_http_client`
- `crates/acmecast-deploy/src/targets/webdav.rs: WebdavTarget::new(proxy_url, timeout)` — WebDAV 部署持有 `global_proxy_url`，`credential_proxy` 改为 `effective_proxy_url(fields, global_proxy_url)`
- `crates/acmecast-deploy/src/targets/tencent.rs: TencentSslTarget::new(proxy_url, timeout)` — 腾讯云 SSL 部署持有 `global_proxy_url` 与 `fallback_http`，`call` 经 `transport_for_credentials` 按凭据选层
- `crates/acmecast-server/src/steps/mod.rs: default_dns_registry(proxy_url, timeout) / default_deploy_registry(proxy_url, timeout)` — DNS 与部署注册表均接收全局代理与超时，透传到各提供商/目标
- `crates/acmecast-server/src/credentials.rs: validate_proxy_url_field` — 凭据保存校验（`proxy_mode=custom` 时校验 `proxy_url`，全类型）
- `crates/acmecast-server/src/handlers.rs` — 各凭据类型注册处 `with_global_proxy`
- `crates/acmecast-server/src/steps/apply.rs: complete_challenge` — `doh_transport_from(proxy_url)` 注入传播等待的解析器传输层；写 `proxy: via` 日志行
- `crates/acmecast-server/src/steps/deploy.rs: format_deploy_targets` — `deploy targets:` 日志行（`id`/`type`/`name`/`remark`，缺省 `—`）
- `crates/acmecast-server/src/run.rs: compact_proxy_logs` — `proxy: ` 行落库前仅保留最后一条
- `docs/04-credentials.md: 凭据的出网代理` / `docs/03-configuration.md: 全局代理` — 用户文档

## 运行历史代理与部署目标日志

运行历史需明示"本步骤是否走了代理"与"部署到哪些目标"，各以**独立一行** `info` 日志呈现，便于视觉判断；均经 `redact` 脱敏，不落凭据明文。

### 代理使用日志（`proxy: ` 行）

- `cert.apply`（DNS 挑战）或 `cert.deploy`（部署目标出网）所用凭据的代理分支真实触发出网时（`proxy_mode=inherit` 且全局代理非空、或 `proxy_mode=custom` 且 `proxy_url` 合法），以及 DoH 解析器代理回退成功时，运行历史对应步骤 MUST 新增一行 `info` `proxy: via <host>` 日志，紧随命中 DNS / 平台信息行之后。
- 直连（`proxy_mode=direct`，或 `inherit` 但全局代理为空）时 MUST NOT 新增该行。
- 该行 MUST 与实际出网一致：`cert.deploy` 中未接入代理的目标类型（如 `local`）MUST NOT 打该行；接入代理的目标类型在代理真实触发时打行、直连时不打。
- 该行沿用"传播等待双轨"精简策略：SSE 实时流保持全量，落库前按 `message` 前缀 `proxy: ` 仅保留最后一条（`rposition` + `retain`）；空 / 单条 / 无代理时 no-op。

### 部署目标信息日志（`deploy targets: ` 行）

- `cert.deploy` 步骤进入执行时 MUST 另起一行 `info` `deploy targets: ` 日志，展示本次部署的目标信息：按目标罗列 `id` / `type` / `name` / `remark`（取自 `CredentialResponse` 的 `name` / `remark` 与部署目标注册表项，经 `redact` 脱敏）。
- 该行仅记录本次步骤实际入库并即将执行的目标（非预览页全量清单）；多目标以 `；` 分隔，名称 / 备注缺省时记 `—`；写在"已部署到目标"平台行之前，不参与精简。

### 实现索引

- `crates/acmecast-server/src/steps/apply.rs` / `crates/acmecast-server/src/steps/deploy.rs` — `proxy: via <host>` 行（经 `acmecast_core::proxy_log_target` 脱敏）
- `crates/acmecast-server/src/steps/deploy.rs: format_deploy_targets` — `deploy targets:` 行（`id`/`type`/`name`/`remark`，缺省 `—`）
- `crates/acmecast-server/src/run.rs: compact_proxy_logs` — `proxy: ` 行落库前仅保留最后一条

## 部署平台化日志（`cert.deploy`）

`cert.deploy` 在部署成功或指纹一致跳过时，"已部署到目标" 的同一 `info` 行**前后追加**平台信息（同一行内，不另起一条日志），平台取自 `input.config` 的脱敏摘要，空值时记 `unknown`，始终经 `redact` 脱敏后输出。

| 目标 `target` | 平台字段 |
|---|---|
| `tencent-ssl` | `credential_id` / `region` |
| `s3` | `bucket` / `region` / `endpoint host` |
| `ssh` | `host:port` 或主机档案标识 |
| `local` | 部署路径 |
| `webdav` | `url host` |
| 其他 / 缺字段 | `unknown` 兜底 |

格式示例：`已部署到目标 <target> (platform=<...>)，写入 <N> 个路径` 或 `... fingerprint=<...> platform=<...>`。`tencent-ssl` 的跳过写入分支（`skipped_write`）与写入分支均在同一行追加 `platform`，`output` 结构化字段中亦补 `platform`。失败分支已含 `target`，可追加 `platform`。

### 实现索引

- `crates/acmecast-server/src/steps/deploy.rs: platform_summary` — 按 `target` 从 `config` 取字段拼接 `platform=<...>`，经 `redact` 脱敏
- `crates/acmecast-server/src/steps/deploy.rs:138-186` — "已部署到目标" 成功/跳过分支同一行前后追加 `platform`

## 证书存储查询与续期归档

`cert.store` 在写入新证书前按域名集合（`domains` 归一化后精确匹配）查询既有记录，用于归档旧证书与判断是否新建记录。当同域名集合已存在多条记录（如首次重签后一条已归档 + 一条活跃）时，查询 MUST 确定性返回**活跃记录**（`archived_at` 为空且 `revoked_at` 为空），而非任意记录。此优先级 MUST 在所有依赖域名集合查询的路径一致生效：续期归档、到期检测、跳过注入。

### 查询排序

`CertRepository::find_by_key`（被 `find_by_domains` 调用）的查询排序为：

```sql
ORDER BY (archived_at IS NULL) DESC,  -- 活跃 (1) 排前，已归档 (0) 排后
         (revoked_at IS NULL) DESC,   -- 未吊销 (1) 排前，已吊销 (0) 排后
         id DESC                       -- 同为活跃时取最新（id 最大）
```

用 `IS NULL` 布尔表达式排序，不依赖数据库默认 NULL 排序行为，确保跨数据库一致。无活跃记录时回退到任意记录。

### 归档守卫

`cert.store` 在归档旧证书前增加防御性检查：若查询到的 `existing` 记录的 `cert_pem_path` 或 `key_pem_path` 已位于 `certs/archived/` 或 `certs/revoked/` 前缀下，跳过归档并记 warn（该记录已是归档/吊销态，非当前活跃证书）。正常流程不应触发，但防止极端并发或数据异常下的错误归档。

### 归档语义

- 同域名集合旧记录且新旧指纹不同时，MUST 将旧证书 4 文件按旧指纹移动到 `certs/archived/<旧指纹>/`，文件名不变，旧记录路径列同步更新为归档后路径，新证书以全新记录入库（不覆盖旧记录）。
- 指纹相同时不新建重复记录；未到期跳过且注入库中证书同样不新建。
- 归档目标已存在时覆盖，不报错。
- 归档失败（源文件缺失或 IO 错误）MUST NOT 阻断新证书写入与新建记录，仅记 warn。
- 连续多次重签时，每次归档的是上一张活跃证书（而非重复归档第一张）。

### 实现索引

- `crates/acmecast-store/src/repository/cert.rs: find_by_key` — 活跃优先排序（`IS NULL` 布尔表达式 + `id DESC`）
- `crates/acmecast-store/src/repository/cert.rs: find_by_domains` — 调用 `find_by_key`，直接受益
- `crates/acmecast-server/src/steps/store.rs: archive_replaced_certificate` — 归档守卫（`certs/archived/` / `certs/revoked/` 前缀检查）
- `crates/acmecast-server/src/steps/apply.rs` — 到期检测与跳过注入受益于查询修正；跳过分支的兼容回退读取与领域级失败见下节

### 材料缺失降级为标记吊销终态

当证书材料缺失（库中记录指向的文件在磁盘上不存在）时，归档与吊销操作 MUST NOT 静默返回成功或使记录停留在活跃态形成孤儿记录。系统统一降级为**本地标记吊销终态**：调用 `CertRepository::mark_revoked`（设置 `revoked_at` 为当前时间、清空 `archived_at`），跳过 CA 吊销与文件移动，响应返回 `archived_now=false`（归档入口）或 `revoked_now=true`（吊销入口）并携带 `material_missing=true` 降级标记。降级后记录进入终态，删除按钮按既有规则可用。

- 归档入口（用户点"归档"）：材料缺失 → `mark_revoked` → 响应 `archived_now=false` + `material_missing=true`。
- 吊销入口（用户点"吊销"）：材料缺失 → 跳过读 fullchain 与 CA 调用 → `mark_revoked` → 响应 `revoked_now=true` + `material_missing=true`。
- 已吊销记录重复点归档/吊销仍走既有幂等返回（`archived_now/revoked_now=false`），不重复降级。
- 告警日志 MUST 包含 `cert_id`、`fingerprint`、缺失路径，便于事后排查。

### 吊销搬迁完整性

吊销把证书材料移入 `certs/revoked/<指纹>/` 时，MUST 把库中记录的 **6 个路径列**所指向的文件（`cert_path`/`ca_path`/`fullchain_path`/`key_path`/`cert_pem_path`/`key_pem_path`）全部搬入吊销目录，MUST NOT 在源目录（`certs/ssl/` 或 `certs/archived/<指纹>/`）遗留任何属于该证书的文件。搬迁完成后库中 6 个路径列一致指向吊销目录下的新位置。

- `FileStore::move_certificate` 优先整目录 rename，不可行时退回逐文件搬迁；逐文件清单从原 `cert_pem`/`key_pem` + `certs/ssl/` 活跃候选扩展为包含库中记录的全部 6 路径源文件。
- 吊销同时清空 `archived_at`（设为 `NULL`），使吊销成为唯一终态——不残留"已归档"状态。MUST NOT 在吊销时删除证书文件。
- `revoked_at` 与 `archived_at` 同时非空时 MUST 显示"已吊销"（吊销为终态，优先于归档）。

### 前端提示区分

归档与吊销的完成提示 MUST 区分两种结果：

- 文件搬迁成功时提示归档/吊销成功。
- 响应 `material_missing=true` 时 MUST 提示"材料缺失，已标记为吊销状态，可直接删除"（warning 级），MUST NOT 展示与真实搬迁成功相同的成功提示。

### 实现索引（续）

- `crates/acmecast-store/src/filestore.rs: move_certificate` — 逐文件搬迁清单扩展为 6 路径（`cert`/`ca`/`fullchain`/`key`/`cert_pem`/`key_pem`），整目录 rename 不变
- `crates/acmecast-server/src/handlers.rs` — 归档接口材料缺失降级（`mark_revoked` + `material_missing`）、吊销接口材料缺失降级（跳过 CA + `mark_revoked`）
- `crates/acmecast-store/src/repository/cert.rs: mark_revoked` — 设置 `revoked_at`、清空 `archived_at`（降级复用）
- `frontend/src/pages/certificates/CertificateDetailPage.tsx` — 按 `material_missing` 区分提示文案
- `frontend/src/pages/certificates/CertificateListPage.tsx` — 列表行归档/吊销降级提示、状态列显示与删除按钮可用性

## 跳过注入兼容回退读取与可诊断失败

`cert.apply` 在 `renew_before_days` 判定为"不在续期窗口"时走跳过分支（以 `skipped` 结束、不触达 CA），需从证书库加载匹配记录并读取其 `cert_pem`/`key_pem` 注入产物，供后续 `cert.store`/`cert.deploy` 使用。该分支的文件读取 MUST 具备兼容回退能力与领域级可诊断失败语义，不再透传裸 OS 错误。

### 兼容回退候选路径

跳过分支读取证书文件 MUST 按固定顺序依次尝试，命中任一可读副本即成功注入：

1. **记录路径**：库中记录的 `cert_pem_path` / `key_pem_path`（新布局 `*.ca.cer`/`*.fullchain.cer`/`key`）。
2. **旧布局回退**：若记录路径缺失，按主域名与通配前缀映射推导旧布局候选（`certs/ssl/<old>.cert.pem` / `<old>.key.pem`，其中 `*.` → `_` / `_wildcard` 前缀），复用 `certificate-file-layout` 的兼容读取路径。
3. **归档副本回退**：若旧布局仍缺失，在 `certs/archived/<指纹>/` 下查找对应文件（仅对当前 `cert_id`/指纹的归档目录，匹配记录指纹即最新活跃指纹时视为同一材料）。

所有候选路径 MUST 经 `FileStore` 路径防逃逸校验（拒绝含 `..` 或绝对路径的越界候选），不直接拼接用户可控输入。

### 回退命中

- 命中任一可读副本即以相同产物键（`cert_pem`/`key_pem`/`domains`/`fingerprint`/`cert_id`）注入 `Artifacts`，不覆盖前序同名产物，不中止流水线。
- 回退命中记一条 `INFO` 日志，含指纹与所用的回退路径，便于排障。
- 流水线继续后续步骤，终态为 `succeeded`，不视为失败。

### 回退全部失败的领域错误

当所有候选路径均不可读或缺失时，MUST 以领域错误明确失败，不透传裸 OS 错误（如 `No such file or directory (os error 2)`）：

- 错误信息 MUST 归因到 `cert.apply` 跳过注入阶段，包含证书标识（`cert_id`/指纹/归一化 `domains`）、已尝试的路径列表与数据目录状态（存在性/可读性摘要）。
- 错误信息 MUST 给出可操作指引：检查数据目录挂载与权限、核对归档/迁移是否完成、或将 `renew_before_days` 设为 `0` 强制重签。
- 步骤日志（`ctx.log_error`）MUST 记录完整尝试列表与各路径失败原因。
- 运行历史终态为 `failed`，步骤状态为 `failed`（非 `skipped`），避免"跳过成功"误导。
- 不静默使用错误材料或跳过部署，后续 `cert.store`/`cert.deploy` 不因缺少产物产生二次迷惑错误。

### 日志收敛

- 回退命中仅一条 `INFO`（指纹+回退路径），全部失败一条 `ERROR`（尝试列表+指引），不为每个候选单独打日志。
- 可观测性与 `pipeline-renewal-lifecycle` 的"跳过可观测"要求对齐：运行历史终态命中回退则 `succeeded`，全部失败则 `failed`。
- 跳过分支以运行成功收尾（运行结束简讯照常投递、该步骤在简讯中标注跳过）；失败分支按既有失败通知规则处理。

### 作用域与不变量

- 本变更不改变阈值判定、域名归一、产物键与幂等/归档规则。
- 兼容回退仅在跳过分支且主路径缺失时触发，正常签发路径无额外 IO（串行 2–4 次文件读取，仅限回退场景）。
- 回退读取复用证书详情/下载/吊销/部署路径中已有的兼容读取能力（`certificate-file-layout` 的旧布局回退 + `certificate-storage` 的归档查找），不在 `apply.rs` 内联重写路径推导，避免与规格漂移。

### 实现索引

- `crates/acmecast-server/src/steps/apply.rs` — 跳过分支兼容回退读取（新布局→旧布局→归档副本）与领域级失败（`cert_id`/指纹/`domains`/尝试路径/指引）
- `crates/acmecast-store/src/filestore.rs` / 证书兼容读取助手 — 回退候选路径推导与 `FileStore` 防逃逸校验
- `crates/acmecast-pipeline` — 错误分类与步骤终态（`failed` 而非 `skipped`）
- `crates/acmecast-server/src/steps/apply.rs: zone_of` / `complete_challenge` — 跳过注入与正常签发共用产物键

## 文件类部署目标 3 文件布局与腾讯云 2 文件区分

文件类部署目标（`local` / `ssh` / `s3` / `webdav`）的部署产物从 2 文件改为 **3 文件**，对齐主存储 `certificate-file-layout` 的 3 文件布局；腾讯云 `tencent-ssl` 为云 API 语义，保持 **2 文件**（fullchain + key），二者显式区分。

### 3 文件集合（文件类目标）

文件类目标默认写入/上传 3 文件，不再单独产出叶子 `cer`：

- `<prefix>.fullchain.cer` ← `chain_pem` 原样
- `<prefix>.ca.cer` ← `chain_pem` 按 PEM 拆链规则（`split_chain` / `ca_pem_of`）生成的剩余链（多张时为首块之后剩余，单张时为**空文件**，与主存储一致）
- `<prefix>.key` ← `key_pem`

其中 `<prefix>` = `domain_file_prefix(primary_domain)`（通配 `*.example.com` → `_wildcard.example.com`），与主存储共用同一函数（`acmecast_cert::domain_file_prefix`），拆链共用 `acmecast_cert::ca_pem_of`。

**本次补充（部署侧对齐）：** 文件类部署目标（`local` / `ssh` / `s3` / `webdav`，`tencent-ssl` 除外）的默认产出 MUST 同样对齐上述 3 文件布局（`fullchain.cer` + `ca.cer` + `key`，不单独产出叶子 `cer`），`fullchain.cer` 为 `chain_pem` 原样，`ca.cer` 按 PEM 拆链规则生成。部署目标的 `ca.cer`/`fullchain.cer` 命名与主存储一致，且 `domain_file_prefix` 规则同步（`*.example.com` → `_wildcard.example.com`）。部署侧路径以 `/` 结尾视为目录文件夹时，补全后的默认文件名 MUST 同为 `<prefix>.fullchain.cer / <prefix>.ca.cer / <prefix>.key`（保存时 `cert.apply` 主域可解析则直接拼 `domain_file_prefix` 前缀，否则 `local/ssh` 回落固定名与 `S3/WebDAV` 回落 `{domain}` 占位——部署时展开），`tencent-ssl` 保持 2 文件（云 API 无路径）不变，不改变证书原有命名。

### 部署目录补全文件名同源（含文件夹模式带前缀）

文件类部署目标（`local/ssh/s3/webdav`，`tencent-ssl` 除外）以 `/` 结尾的目录输入补全时，默认文件名 MUST 与 `certs/ssl/` 落盘命名同源：`<prefix>.fullchain.cer`、`<prefix>.ca.cer`、`<prefix>.key`（`<prefix>` = `domain_file_prefix(primary_domain)`，保存时同流水线 `cert.apply` 主域可解析则直接拼前缀，否则回落与部署时按主域展开），确保用户 `fullchain/key/ca` 三字段均填 `"data/certs/ssl/"` 或 `"/tmp/t1/"` 这类目录时按当前证书主域补全为带前缀的 3 文件。通配 `*.example.com` 时前缀为 `_wildcard.example.com`。

### 腾讯云 2 文件语义（对比声明）

`tencent-ssl` 部署目标保持 2 文件语义：上传参数 `CertificatePublicKey=chain_pem`（fullchain）与 `CertificatePrivateKey=key_pem`，不涉及文件化与 `ca.cer` 拆分。文档与校验口径 MUST 显式声明该差异，避免与文件类目标的 3 文件混淆——实现上 MUST NOT 引入 `ca.cer` 拆分逻辑。

### 字段与别名兼容

各目标新增 `fullchain`/`ca` 路径或模板字段，存量 `cert_path`/`cert_filename` 视为 `fullchain` 的别名（`fullchain_path`/`fullchain_filename` 未配置时生效）；`ca` 路径未显式配置时按 fullchain 同目录同前缀派生 `*.ca.cer`，**显式留空**可跳过 `ca.cer` 产出。打开存量流水线步骤时 `fullchain_path`/`fullchain_filename` 自动回显 `cert_path`/`cert_filename` 值。

**`local` 目标为 BREAKING 替换而非并存**：`LocalInput` 的 `cert_path` 字段已移除，`fullchain_path` 直接替换之（`#[serde(alias = "cert_path")]` 兜底旧输入反序列化）；schema 中不再出现 `cert_path`。前端 `migrateLegacyDeployInput` 已把存量 `cert_path` 回显为 `fullchain_path`、`ca_path` 回显派生的 `ca.cer` 路径，保存后落新字段。由于 serde `alias` 把新旧键视为同一字段，存量步骤回显后保存会同时带上两个键，后端在解析前经 `without_legacy_alias` 去重（`fullchain_path` 非空时丢弃 `cert_path`，新键优先），避免 `duplicate field` 报错。

| `local` | `fullchain_path` / `ca_path` | `cert_path` | `ca_path` ← fullchain 同目录 `*.ca.cer` |
| `ssh` | `fullchain_path` / `ca_path` | `cert_path` | 无派生——`ca_path` 缺省/留空即跳过 ca 文件（不按 `fullchain_path` 派生） |
| `s3` | `fullchain_filename` / `ca_filename` | `cert_filename` | `ca_filename` ← `<prefix>.ca.cer` |
| `webdav` | `fullchain_path` / `ca_path` | `cert_path` | `ca_path` ← `<prefix>.ca.cer` |

### 流水线保存时目录补全与持久化（含文件夹模式带前缀）

文件类目标（`local / ssh / s3 / webdav`，`tencent-ssl` 除外）的路径/文件名字段以 `/` 结尾视为目录文件夹，文件夹模式下 `fullchain / ca / key` 三字段分别独立补全。`PipelineRepository::save` MUST 在校验与入库前对 `cert.deploy` 步骤的 `config` 做补全，并以补全后的值持久化（前端回显即为部署实际使用的路径），`tencent-ssl` 分支 MUST 跳过（无路径字段）。

#### 目录判据

系统 MUST 以 `value.trim().ends_with('/')` 为唯一目录判据；仅当值以 `/` 结尾时视为目录并触发补全，非 `/` 结尾、空白、缺省的值 MUST 保持原样不补全，不做 `is_dir()` 探测。

#### 补全规则（保存时按主域前缀）

补全时优先取同流水线 `cert.apply` 步骤的 `domains[0]` 或关联证书主域，经 `domain_file_prefix` 派生 `<prefix>`（通配 `*.example.com` → `_wildcard.example.com`）：

| 目标 | 目录字段 | 主域可解析 | 主域缺失 |
|---|---|---|---|
| `local` / `ssh` | `fullchain_path`（含旧别名 `cert_path`）→ `<目录><prefix>.fullchain.cer`，`ca_path` → `<目录><prefix>.ca.cer`，`key_path` → `<目录><prefix>.key` | 直接拼带前缀文件名并入库，如 `*.example.com` + `/tmp/t1/` → `/tmp/t1/_wildcard.example.com.fullchain.cer` 等 | 回落固定名 `fullchain.cer / ca.cer / key`，如 `/tmp/t1/` → `/tmp/t1/fullchain.cer` |
| `s3` | `prefix` 归一化为 `<prefix>/`；`fullchain_filename`/`cert_filename` → `<目录><prefix>.fullchain.cer`，`ca_filename` → `<目录><prefix>.ca.cer`，`key_filename` → `<目录><prefix>.key` | 同上直接拼前缀，如 `example.com` + `certs/` → `certs/example.com.fullchain.cer` | 回落 `{domain}` 占位模板 `"<目录>{domain}.fullchain.cer"` 等，部署时按 `domain_file_prefix(primary_domain)` 展开 |
| `webdav` | `base_path` 归一化为 `<base>/`；`fullchain_path`/`cert_path` → `<目录><prefix>.fullchain.cer`，`ca_path` → `<目录><prefix>.ca.cer`，`key_path` → `<目录><prefix>.key` | 同上 | 回落 `{domain}` 占位，部署时展开 |

文件夹模式三字段均支持：通配 `*.example.com` 时 `fullchain_path="/tmp/t1/"`、`key_path="/tmp/t1/"`、`ca_path="/tmp/t1/"` 分别补为 `/tmp/t1/_wildcard.example.com.fullchain.cer` / `.ca.cer` / `.key`；相对路径 `certs/ssl/` 同样以 `/` 结尾触发补全。

#### 校验与持久化时序

`PipelineRepository::save` MUST 先执行目录补全归一化，再执行后续校验与持久化；补全后的完整路径参与必填等校验并写入 `pipeline_step.input`。`tencent-ssl` 目标的 `config` 含路径键时不触发补全，原样入库。

#### 执行期兜底与文件名前缀不变

部署执行期若路径仍以 `/` 结尾（历史存量未重保存），MUST 按 `domain_file_prefix(primary_domain)` 动态补全为 `<prefix>.fullchain.cer / <prefix>.ca.cer / <prefix>.key`，文件名 MUST 与 `certs/ssl/` 落盘命名同源。`S3`/`WebDAV` 的 `ca` 内容为空（单张链）时跳过对应 PUT，但 `DeployOutcome.paths` 仍列出 3 个预期键。

#### 前端说明

`GET /api/tasks/cert.deploy/schema` 的 `x-target-schemas` 与前端表单提示 MUST 说明：`fullchain/ca/key` 路径以 `/` 结尾视为目录文件夹模式，保存时按当前证书主域自动补全为 `<prefix>.fullchain.cer` 等带前缀文件名（通配 `*.example.com` → `_wildcard.example.com.*`，`tencent-ssl` 不参与）。

### local 三文件路径、权限与目录/留空语义

`local` 目标的部署输入形成 fullchain/ca/key 三组「路径 + 权限」对：`fullchain_path` + `cert_mode`（fullchain 权限，保留 `cert_mode` 原名以对齐前端 `LOCAL_FIELD_ORDER`）、`ca_path` + `ca_mode`（默认 `0644`）、`key_path` + `key_mode`（默认 `0600`）；`uid`/`gid`/`reload_command` 不变。

**表单渲染**：`cert.deploy` 选择 `local` 目标时，`config` 子表单 MUST 渲染 `fullchain_path`/`ca_path`/`key_path` 三路径与 `cert_mode`/`ca_mode`/`key_mode` 三权限，**三行两列**排列——每行左列为文件路径、右列为文件权限（`key_path`+`key_mode`、`fullchain_path`+`cert_mode`、`ca_path`+`ca_mode` 各占一行），顺序由前端 `LOCAL_FIELD_ORDER = ["key_path","key_mode","fullchain_path","cert_mode","ca_path","ca_mode","uid","gid","reload_command"]` 钉住（key 行在最上）。表单提示 MUST 明确该目标将部署 `fullchain.cer`/`ca.cer`/`key` 三文件，未自定义 `ca_path` 时按 `fullchain_path` 派生。

**部署写入**：配置 `fullchain_path`/`ca_path`/`key_path` 均为文件路径并触发部署时，MUST 写入三个文件——`fullchain_path` 写入完整证书链（`chain_pem` 原样）、`ca_path` 写入中间链（`ca_pem_of(chain_pem)`）、`key_path` 写入私钥（`key_pem`），成功返回的路径集合即三者（ca 跳过时除外）。

- **目录路径按默认文件名写入**：任一路径以 `/` 结尾或 `is_dir()` 为 true 时视为目录，拼接默认文件名写入该目录——fullchain → `fullchain.cer`、ca → `ca.cer`、key → `key`；为文件路径时原样写入（`LocalInput::files` 中 `resolve_target_path` 实现）。
- **ca 跳过规则**：`ca_path` 显式留空（空串）时 MUST 跳过 `ca.cer` 文件写入，仅写入 fullchain 与 key 两个文件；`ca_pem_of(chain_pem)` 拆链为空（链仅含叶子证书）时同样跳过，不产生空文件（与显式留空一致）。`ca_path` 未配置（`None`）时按 `fullchain_path` 同目录派生 `<stem>.ca.cer`（`derive_ca_path`，如 `example.com.fullchain.cer` → `example.com.ca.cer`、`fullchain.cer` → `ca.cer`）。
- **存量兼容**：仅含 `cert_path`/`key_path` 的旧步骤打开时，`fullchain_path` MUST 回显 `cert_path` 的值，`ca_path` 回显按 `fullchain_path` 派生的 `ca.cer` 路径（前端 `migrateLegacyDeployInput` + 后端 `#[serde(alias = "cert_path")]` 与 `without_legacy_alias` 去重，`fullchain_path` 非空时丢弃 `cert_path`）。
- 文件清单由 `LocalInput::files` 返回 `Vec<(FileSpec, String)>`（ca 可选，数量可变），`deploy` 方法按可变清单分配容量与遍历；`example_input` 为三文件示例 `{"fullchain_path":"/etc/nginx/ssl/example.com.fullchain.cer","ca_path":"/etc/nginx/ssl/example.com.ca.cer","key_path":"/etc/nginx/ssl/example.com.key","reload_command":"nginx -s reload"}`，`input_schema` 由 `schemars::JsonSchema` 从 `LocalInput` 自动派生，不再含 `cert_path`。
### SSH 三文件路径、目录/留空语义、表单两行两列与 allowClear

`cert.deploy(ssh)` 的远端写入从 2 文件改为最多 3 文件，**BREAKING** 替换 `SshInput.cert_path: Option<String>` 为 `fullchain_path: Option<String>`（`#[serde(alias = "cert_path")]` 兼容存量，`#[serde(default)]` 的 `ca_path: Option<String>` 新增），`ResolvedSshConfig` 同步由 `cert_path: String` 改为 `fullchain_path: String` 并新增 `ca_path: Option<String>`；其余 `key_path`/`cert_mode`/`key_mode`/`credential_id`/`bastion_credential_id`/`host`/`port`/`user`/`auth`/`reload_command` 不变。`ca` 权限复用档案的 `cert_mode`（缺省 `0644`），不新增 `ca_mode`——SSH 变体不渲染权限字段，权限由档案提供。

**输入与档案合并校验**：`resolve_input` 中 `fullchain_path` 与 `key_path` 为必填——值为 `None` 或空白（`trim().is_empty()`）时 MUST 在执行前失败并定位到对应字段名；`ca_path` 为可选——`None` 或空白均视为跳过 ca 文件，不报错。远端文件路径与 `reload_command` 仅来自部署输入，不参与档案合并；输入显式值覆盖档案默认值（端口 22、`cert_mode 0644`/`key_mode 0600`），未给出时回退档案值。不引用档案的直填输入保持兼容，仅路径字段由 `cert_path` 经 alias 映射到 `fullchain_path`。

**部署写入**：`SshTarget::deploy` 构建 `Vec<(String,String,String)>`（`path, content, mode`）写入远端——`fullchain_path` 经 `resolve_remote_path(&fullchain_path,"fullchain.cer")` 写入 `chain_pem`（`mode=cert_mode`），`ca_path` 非空时经 `resolve_remote_path(ca_path,"ca.cer")` 写入 `ca_pem_of(&chain_pem)`（空串跳过，`mode=cert_mode`），`key_path` 经 `resolve_remote_path(&key_path,"key")` 写入 `key_pem`（`mode=key_mode`）；`ca_path` 留空/缺失或 `ca_pem_of` 为空（单证书链）时 MUST 仅写入 fullchain 与 key。`DeployOutcome.paths` MUST 列出实际写入的路径（2 或 3 个），`DeployMode::SkipWrite` 时不发起写入但仍列出预期路径。任一文件失败则整体失败，已写临时文件清理（原子写 `write_file`）。

- **远端目录按默认文件名写入**：SSH 无法本地 `is_dir()`，仅以 `/` 结尾判定为目录——`resolve_remote_path(path, default_name)` 若 `path.ends_with('/')` 则 `format!("{path}{default_name}")`，否则原样返回；fullchain → `fullchain.cer`、ca → `ca.cer`、key → `key`。与 `local` 的 `is_dir() || ends_with('/')` 区分，不执行远端 `test -d`。
- **存量兼容与去重**：存量仅含 `cert_path` 的步骤经 `#[serde(alias="cert_path")]` 映射到 `fullchain_path`；`deploy` 在 `parse_input` 前调用 `without_legacy_alias`（与 `local` 同逻辑，`fullchain_path` 非空时移除旧 `cert_path` 键，新键优先），避免双键 `duplicate field`。前端别名回显已覆盖 `fullchain_path` 回显 `cert_path`。
- **`example_input` 与 schema**：`example_input` 更新为三文件示例 `{"credential_id":1,"fullchain_path":"/etc/ssl/example.com.fullchain.cer","ca_path":"/etc/ssl/example.com.ca.cer","key_path":"/etc/ssl/example.com.key","reload_command":"systemctl reload nginx"}`，`input_schema` 由 `schemars::JsonSchema` 从 `SshInput` 自动派生，不再含 `cert_path`。

**表单两行两列**：`cert.deploy` 选择 SSH 远程主机时，`config` 子表单 MUST 仅渲染档案引用与部署输入——`fullchain_path`/`ca_path`/`key_path` 三远端路径与 `reload_command`，不渲染 `host`/`port`/`user`/`auth`/`cert_mode`/`key_mode`/`cert_path`（由档案提供）。字段顺序由 `SSH_FIELDS = ["credential_id","bastion_credential_id","key_path","fullchain_path","ca_path","reload_command"]` 钉住，`tightenSshSchema` 中仅 `credential_id` 与 `bastion_credential_id` 设 `x-end-row`（各独占一行右侧空列），`reload_command` 不再独占——`key_path`+`fullchain_path` 同行（key 左、fullchain 右）、`ca_path`+`reload_command` 同行（ca 左、reload 右），两行两列；`credential_id`/`bastion_credential_id` 各独占一行。

**跳板与 allowClear**：`bastion_credential_id` 的 `x-visible-when: Not{Empty:"credential_id"}`、`x-options` 动态排除已选目标、`x-label="跳板机"` 保持不变；所有以 `x-options` 渲染的单选 `Select`（含 `credential_id`/`bastion_credential_id` 等）SHALL 启用 `allowClear`，用户可清空已选值回退到 `null`/缺省，清空后提交的 `config` 对应字段为 `null` 或缺失、`bastion_credential_id` 清空即不走跳板。多选与 `enum` 下拉不加。`bastion` 清空后部署直连目标主机。


- `local`/`ssh`：3 文件走原子写（临时文件 + `rename`）、`parse_mode`/`set_mode`/`set_owner` 校验；任一文件失败则整体失败，已写临时文件清理。
- `s3`/`webdav`：任一 PUT 失败即失败，不污染主存储；`DeployMode::SkipWrite`（指纹一致且非 `force`）时 3 文件路径均列入 `DeployOutcome.paths`，不发起写入。
- 运行历史与部署日志中引用的路径 MUST 仅包含实际部署的 3 文件（`*.ca.cer` / `*.fullchain.cer` / `*.key`），MUST NOT 引用不存在的叶子 `*.cer`。

### 实现索引

- `crates/acmecast-cert/src/pem.rs: split_chain / ca_pem_of` — 拆链纯函数（主存储与部署侧共用）
- `crates/acmecast-cert/src/lib.rs: domain_file_prefix` — 公共前缀派生（`*.example.com` → `_wildcard.example.com`）
- `crates/acmecast-store/src/repository/pipeline.rs: normalize_deploy_inputs / normalize_deploy_config` — 保存时按 target 补全目录写法并持久化
- `crates/acmecast-deploy/src/targets/local.rs: LocalInput / files / resolve_target_path / normalize_local_config / derive_ca_path / without_legacy_alias` — `fullchain_path`/`ca_path`/`key_path` 三组路径+权限、目录路径按默认文件名写入、`ca_path` 派生与留空/拆链空跳过、`cert_path` 别名去重
- `crates/acmecast-deploy/src/targets/ssh.rs: SshInput / ResolvedSshConfig / resolve_input` — 同 local 的别名回填与派生逻辑
- `crates/acmecast-deploy/src/targets/s3.rs: S3Input` — `fullchain_filename`/`ca_filename`/`key_filename` 与 `cert_filename` 别名
- `crates/acmecast-deploy/src/targets/webdav.rs: WebdavInput` — `fullchain_path`/`ca_path`/`key_path` 与 `cert_path` 别名
- `crates/acmecast-deploy/src/targets/tencent.rs` — 2 文件上传（`CertificatePublicKey=chain_pem` + `CertificatePrivateKey=key_pem`），不拆 `ca.cer`
- `docs/07-deploy-targets.md` — 各目标字段表与共享 3 文件语义

## S3 与 WebDAV 部署目标（存储类目标）

`s3` 与 `webdav` 为存储类文件部署目标，默认产出同为 3 文件（`fullchain.cer / ca.cer / key`），字段与 `local/ssh` 的目录补全规则同源，差异仅为对象键/URL 语义与前缀归一化。

### S3 部署目标

`s3` 将证书材料以对象形式写入 S3 兼容存储。写入对象由 3 文件派生：默认对象键为 `<prefix>.fullchain.cer / <prefix>.ca.cer / <prefix>.key`（`<prefix>` = `domain_file_prefix(primary_domain)`，通配 `_wildcard`），不单独产生叶子 `cer`。输入字段：`credential_id`（必填，`deploy.s3` 凭据）、`prefix`（可选，对象键前缀）、`cert_filename`/`fullchain_filename`/`ca_filename`/`key_filename` 模板（可选，`cert_filename` 视为 `fullchain_filename` 别名）、`force_path_style`/`insecure_skip_verify`/`force` 均可选。凭据中取 `access_key`/`secret_key`（可选 `session_token`），流水线 input 不存明文。

- 未自定义模板时对象键按主域派生 `fullchain.cer / ca.cer / key`，后缀对齐主存储，不产生旧 `cert.pem / key.pem`，亦不单独产生叶子 `cer`；自定义模板的 `{domain}` 占位替换为新 `prefix`。
- `prefix` 归一化：去空白与首尾 `/` 后为空则 `""`，否则补 `/` 后缀（如 `acme` → `acme/`）；对象键为 `<prefix>{domain}.fullchain.cer` 等拼接结果。
- 目录输入保存时按主域前缀补全：`cert_filename / fullchain_filename / ca_filename / key_filename` 以 `/` 结尾视为目录，保存时按同流水线 `cert.apply` 主域经 `domain_file_prefix` 补全为 `"<目录><prefix>.fullchain.cer"` 等并持久化，主域缺失时回落 `"<目录>{domain}.fullchain.cer"` 占位、部署时展开；`ca.cer` 为空（单张链）时跳过 PUT 但 `paths` 仍列出。
- 存量仅含 `cert_filename` 的配置视为 `fullchain_filename` 别名，`ca.cer` 按默认派生。

### WebDAV 部署目标

`webdav` 将证书材料写入 WebDAV，默认路径为 `<prefix>.fullchain.cer / <prefix>.ca.cer / <prefix>.key` 3 文件（通配 `_wildcard`）。输入字段：`credential_id`（必填，`deploy.webdav` 凭据）、`base_path`（可选）、`cert_path`/`fullchain_path`/`ca_path`/`key_path` 模板（可选，`cert_path` 视为 `fullchain_path` 别名）、`insecure_skip_verify`/`force` 均可选。`ca.cer` 按拆链规则生成。

- `base_path` 归一化同 `prefix`（空则 `""`，否则 `<base>/`）。
- 路径/模板字段以 `/` 结尾视为目录：保存时 MUST 按同流水线 `cert.apply` 主域补全为目录 + `<prefix>.fullchain.cer / <prefix>.ca.cer / <prefix>.key` 并持久化，主域缺失时回落 `{domain}` 占位、部署时展开；`ca_path` 目录同样触发补全。

### 路径拼接单斜杠归一化

`s3` 与 `webdav` 拼接 `prefix` / `base_path` 与文件路径（含以 `/` 开头的绝对路径）时，衔接处 MUST 只有且仅有一个 `/`，路径内部的连续 `/` MUST 一并折叠；协议部分（`s3://` / `https://`）单独组装，不受归一化影响。实现由 `crates/acmecast-deploy/src/targets/mod.rs::join_path(base, path)` 统一：剥 `base` 尾部 `/` 与 `path` 头部 `/` 后单 `/` 衔接，`webdav_paths` 的 URL 与 S3 的 3 个展示键 + 实际 `PUT` 键共 4 个拼接点全部接入。

### 目录补全持久化与执行期兜底

文件类目标的路径字段以 `/` 结尾视为目录时，系统 MUST 在 `PipelineRepository::save` 阶段即补全并写回 DB，前端回显即为补全后的完整路径；部署执行期 MUST 保留以 `/` 结尾的兜底补全，确保未重保存的历史配置仍可部署成功。保存后回显与部署时动态补全的文件名同为 `<prefix>.fullchain.cer / <prefix>.ca.cer / <prefix>.key`，与 `certs/ssl/` 落盘命名同源。

### S3/WebDAV 上传超时与代理继承

S3 与 WebDAV 的部署上传（S3 `PutObject`、WebDAV `MKCOL`/`PUT`）MUST 受 `[network] timeout_secs` / `ACMECAST_NETWORK_TIMEOUT_SECS` 超时约束（默认 30 秒），超时以可读错误失败（S3 含 bucket 与对象键、WebDAV 含 URL），绝不永久挂起。跳过写入（`skipped_write`）时不发起网络调用，不受超时约束；目录补全不改变超时语义，超时仍以最终补全后的对象键/URL 计时与报错。凭据 `proxy_mode=inherit` 时 MUST 继承全局代理（`[proxy] url` / `ACMECAST_PROXY_URL`），与 DNS 提供商同口径。

## 部署目录补全的执行可靠性与日志增强

承接上节目录补全，`cert.deploy` 在执行期的可靠性与可观测性按本节增强，覆盖 SSH 写入可见性、逐行路径日志、指纹收敛，与目录补全的持久化/兜底同源（`domain_file_prefix`，通配 `*.example.com` → `_wildcard.example.com`）。

### SSH 远端写入失败必须可见

`ssh` 目标的远端命令通道 MUST 只以 `ExitStatus` 为终态判定结果——`Eof` 先于退出码到达时 MUST NOT 提前结束等待并把退出码默认为 0；命令不发退出码时由既有超时兜底。远端写入脚本 MUST 在写入前 `mkdir -p '<父目录>'`（与 `local` 的 `create_dir_all` 同口径），任何一步失败 MUST 以 `远端写入失败（退出码 N）: …` 的可读错误失败，MUST NOT 误报部署成功。实现上远端写入脚本提取为 `remote_write_script`，顺序为 `mkdir -p` → 临时写入 → 替换，确保 `/tmp/t1/` 这类目录补全后的父目录不存在时自动创建。

### 步骤日志逐行列出传输目标路径

`cert.deploy` 步骤日志对非 `tencent-ssl` 目标（`local / ssh / s3 / webdav`）MUST 在 `已部署到目标 <target>` 摘要行之后逐行列出本次传输的目标路径（每个文件一行，形如 `  → <路径>`，与 `DeployOutcome.paths` 一致，含跳过写入时）；`tencent-ssl` 为云 API 语义（无文件路径）MUST NOT 输出逐行路径，仅保留结构化证书 ID 输出。该逐行日志便于按路径到目标上逐一核对。

### SSH 证书写入先于重载且失败不掩盖

`SshTarget::deploy` 在 `DeployMode::Write` 时 MUST 先完成全部远端文件写入（`fullchain.cer`/`ca.cer`/`key`，`ca` 可选）再执行 `reload_command`，任一 `write_file` 失败 MUST 以可读错误整体失败且不再执行重载，避免“重载成功掩盖写入失败”；跳板与直连路径的写入清单同为 `ssh_files()` 展开结果，目录补全（`/tmp/t1/` → `_wildcard.*`）在跳板链路同样生效。

### SSH 跳板机使用写入运行日志

当 `cert.deploy` 的 `target` 为 `ssh` 且 `config.bastion_credential_id` 非空时，系统 MUST 在 `deploy targets:` 与可选的 `proxy: via ...` 日志之后写入一条 `bastion: via <host[:port]> (id=...)` 的信息日志以表明经跳板连接，失败分支亦保留；直连（`bastion_credential_id` 为空/缺失）时 MUST NOT 输出该行，日志不含口令或密钥（与上节跳板机章节的引用与连接流程一致，此处为日志可观测性约束）。

### 非 cloud.tencent 目标每次上传不检测指纹

除 `target` 为 `cloud.tencent` / `tencent-ssl` 外，所有 `cert.deploy` 目标（`ssh`/`local`/`s3`/`webdav` 等）MUST 每次均以 `DeployMode::Write` 执行，不检测指纹是否一致，不触发 `skipped_write`；仅 `cloud.tencent` / `tencent-ssl` 在指纹与上次一致且未强制时可跳过写入但仍执行重载。存量 `DeploymentEntry` 的旧指纹保留但不再参与非腾讯云目标的判定。

### 实现索引（本节）

- `crates/acmecast-deploy/src/targets/ssh.rs: run_channel / remote_write_script` — 仅认 `ExitStatus`，`mkdir -p` 父目录
- `crates/acmecast-server/src/steps/deploy.rs: CertDeployStep::execute` — 逐行 `→ <路径>` 日志（非 `tencent-ssl`）与 `bastion: via` 日志
- `crates/acmecast-deploy/src/targets/ssh.rs: SshTarget::deploy` — 写入先于重载、失败短路
- `crates/acmecast-deploy/src/deployer.rs: decide_mode` — 仅 `tencent-ssl` / `cloud.tencent` 走指纹一致判定，其余强制 `Write`
- `crates/acmecast-deploy/src/targets/mod.rs: join_path` — 单斜杠归一化（与上节 S3/WebDAV 拼接共用）


当目标主机不在公网或被防火墙隔离、仅可通过一台跳板机访问时，`cert.deploy(ssh)` 支持引用另一份 `deploy.ssh` 凭据作为跳板机，经跳板机的 SSH `direct-tcpip` channel 转发到目标主机。跳板是流水线步骤层面的组合，`deploy.ssh` 凭据类型与 `SshHostFields` 字段不变。

### 跳板凭据引用（`bastion_credential_id`）

- `SshInput` 新增可选字段 `bastion_credential_id: Option<i64>`（`#[serde(default)]`），引用另一份 `deploy.ssh` 凭据作为跳板机。留空或缺失表示不使用跳板，部署直连目标主机。
- `bastion_credential_id` 与顶层 `credential_id`（目标主机档案）SHALL NOT 引用同一份凭据；相同时部署 MUST 在执行前失败，错误指出「目标与跳板不可引用同一份凭据」并定位到 `bastion_credential_id` 字段。
- 跳板凭据提供跳板机的连接信息（host/port/user）与认证材料；目标凭据提供目标主机的连接信息与认证材料，二者独立解析、独立认证。
- 跳板凭据不存在时部署执行前 MUST 失败，报错包含被引用的凭据标识并定位到 `bastion_credential_id` 字段。

### 独立解析与认证

- 执行时系统分别解析跳板凭据与目标凭据，各自取出 `SshHostFields`（host/port/user/auth/proxy_url），互不影响。跳板凭据的认证材料在执行时从凭据系统解出，MUST NOT 出现在流水线配置或运行历史中。
- 跳板路径与目标路径各自使用对应凭据的出网代理配置（`proxy_mode` / `proxy_url`）。跳板凭据配了代理时先经代理隧道连跳板机，再从跳板机转发到目标主机；跳板到目标的 `direct-tcpip` 转发段不走代理。
- 跳板凭据字段校验复用 `SshHostFields::validate`，校验报错口径独立指向 `bastion_credential_id.*`，与目标档案的问题分开报。
- `resolve_input` 签名新增 `bastion_profile: Option<&SshHostFields>` 参数，非空时构建 `BastionConfig`（host/port/user/auth/proxy_url）。

### 连接流程

配置了跳板机时 SSH 连接分两步：

1. 先 SSH 连接跳板机并用跳板凭据的材料认证（如有代理则先建代理隧道）。
2. 在跳板机会话上开 `direct-tcpip` channel（RFC 4254）转发到目标主机，channel 转为双向流后在其上建立目标 SSH 会话并用目标凭据的材料认证。
3. 目标会话完成文件写入与重载命令执行。

跳板 Handle 随 transport 一起存活（`_bastion_session: Option<Handle>`），防止跳板会话被提前 drop 导致转发流断开。直连路径 `_bastion_session` 为 `None`，行为不变。

### 超时口径

- 跳板连接 + 跳板认证：单个 timeout（与既有 `connect_ssh` + 认证口径一致）。
- `direct-tcpip` channel 开 + `connect_stream` + 目标认证：单个 timeout。
- 文件写入 / 命令执行：各自 timeout（既有逻辑不变）。
- 叠加代理时，代理 TCP 连接阶段不计时（既有行为），计时从代理隧道 TCP 流建立完成后开始。
- 超时错误 MUST 包含跳板机与目标主机信息（地址 + 端口）以助排障，跳板失败含「跳板机」前缀，目标失败含「目标主机」前缀。

### 前端步骤表单联动

- SSH 变体子表单新增 `bastion_credential_id` 字段（标签「跳板机」），排在 `credential_id`（SSH 主机档案）之后、路径字段之前，设 `x-end-row`（独占一行右侧空列）。
- 该字段仅在 `credential_id` 已选择后显示（`x-visible-when: { Not: { Empty: "credential_id" } }`，`Condition` 新增 `Not` 变体取反）；未选目标时不渲染。
- 跳板下拉选项列出所有 `deploy.ssh` 凭据，SHALL 排除当前已选为目标（`credential_id`）的那一项；目标切换时选项刷新（`Form.useWatch` 监听 `credential_id` 变化重算 `x-options`）。
- `bastion_credential_id` 默认留空（不使用跳板），SHALL NOT 为必填。保存时留空则写入 null。
- 连通性测试（`probe_host`）不涉及跳板——测的是单台主机档案。

### 作用域与不变量

- 不支持多级跳板（ProxyJump 链），仅单级。
- 不在凭据中新增跳板字段——跳板是部署时的组合。
- 不引入 known_hosts / 主机指纹校验。
- 跳板功能不改变 3 文件布局、别名回填、原子写入与权限校验等既有逻辑。

### 实现索引

- `crates/acmecast-deploy/src/targets/ssh.rs: SshInput` — `bastion_credential_id: Option<i64>` 字段
- `crates/acmecast-deploy/src/targets/ssh.rs: BastionConfig` — 跳板执行配置（host/port/user/auth/proxy_url）
- `crates/acmecast-deploy/src/targets/ssh.rs: ResolvedSshConfig` — 新增 `bastion: Option<BastionConfig>`
- `crates/acmecast-deploy/src/targets/ssh.rs: resolve_input` — 新增 `bastion_profile` 参数，构建 `BastionConfig`，校验报错指向 `bastion_credential_id.*`
- `crates/acmecast-deploy/src/targets/ssh.rs: SshTarget::deploy` — 解析跳板凭据、校验不可与目标相同、传给 `resolve_input`
- `crates/acmecast-deploy/src/targets/ssh.rs: RusshTransport` — `_bastion_session: Option<Handle>` 持有跳板会话
- `crates/acmecast-deploy/src/targets/ssh.rs: RusshConnector::connect_via_bastion` — 两跳连接与认证，错误区分跳板/目标前缀
- `frontend/src/pages/pipelines/PipelineEditorPage.tsx: SSH_FIELDS / SSH_LABELS / tightenSshSchema / withFieldOptions` — 跳板字段渲染、`x-visible-when`、`x-end-row`、`x-options` 动态排除目标
- `frontend/src/components/SchemaForm.tsx: Condition` — `Not` 变体与 `conditionSatisfied` 取反
