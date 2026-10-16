# AcmeCast 项目规范（SPEC）

本文件为全局规范，涵盖 ACME 账号、DNS 校验与证书生命周期、凭据出网代理与全局代理配置、运行历史可观测性等产品行为，`docs/SPEC.md` 为唯一真实来源。下文首节为 DNS Zone 推导，其余为内建 CA、凭据代理、日志规范等全局约定。

## DNS Zone 自动推导

流水线 `cert.apply` 的 `dns_zone` 为空时按本节推导；无法推导或多域时置为必填。

### 术语

- `domains`：流水线输入的域名列表，元素为 `example.com` 或 `*.example.com` 通配形式。
- `dns_zone`：挑战记录写入的托管区域名称，用于构造 `_acme-challenge.<dns_zone>` 的写入位置。
- “合法 zone”：含至少一个点、非首尾为空段的域名，如 `example.com`、`gcli.cn`、`x.idev.top`。

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

且前端 `cert.apply` 的 `dns_zone` 字段在 `domains.length >= 2` 时标记为必填（`x-required-when: MinItems(field=domains,count=2)`）。

#### R5 单域推导的通用口径
若 `domains` 为单域且 `dns_zone` 未设置，推导口径为“多域名中的域名去除第一个点前的前缀”：

- 非通配：取第一个点后的剩余（`xx.xxx.xx` → `xxx.xx`）
- 通配：等同 R1 的“去 `*.`”（`*.xxx.xx` → `xxx.xx`）
- 该值须为合法 zone，否则按 R4 报错。

#### R6 跨 zone 委托
当挑战记录写入位置与 CA 侧查询名分离（`_acme-challenge.<dns_zone>` 与 `_acme-challenge.<授权域名>` CNAME 委派），`dns_zone` 按托管实际位置填写，提供商侧写入名由 `challenge_write_name(_acme-challenge.<dns_zone>)` 与 CA 查询名 `challenge_record_name(_acme-challenge.<identifier>)` 分别构造。

#### 实现索引

- `crates/acmecast-server/src/steps/apply.rs: zone_of` — 单标签剥离与验证
- `crates/acmecast-server/src/steps/apply.rs: resolve_wildcard_zone` — 多级通配的逐级探测，`probe` 以 `find_txt` 误码 `NoExist/找不到 zone/not exist/InvalidDomainName` 判定托管
- `crates/acmecast-server/src/steps/provider_select.rs` — `dns_credential_id` 未指定时按注册表稳定序遍历 DNS 类凭据自动选择
- 前端校验：`cert.apply` 的 `dns_zone` 在多域时必填

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

以下 8 类凭据的字段顺序、分组与宽度标注为唯一真实来源，前端按此渲染，不在渲染器内写类型分支。`仅一行`均指独占一行（`x-full-width` 整行或半宽 `x-end-row` 右侧空占位，见宽度标注）。

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

### 实现索引

- `crates/acmecast-access/src/credential/acme_account.rs: AcmeAccountType::fields_schema` — `x-field-order` + `ca.x-end-row` + `directory_url.x-full-width` + `credentials.x-multiline`
- `crates/acmecast-access/src/credential/webdav.rs: webdav_fields_schema` — `x-field-order: ["url","base_path","username","password","proxy_mode","proxy_url"]`
- `crates/acmecast-access/src/credential/s3.rs: s3_fields_schema` — `x-field-order` 8 字段 + `session_token` 由代理注入补 `x-end-row`
- `crates/acmecast-deploy/src/ssh_host.rs: ssh_host_fields_schema` — `x-field-order` 9 字段 + `private_key.x-multiline`
- `crates/acmecast-dns/src/providers/tencent.rs: tencent_credentials_schema` — `account_site.x-end-row` 先于代理注入，`secret_id`｜`secret_key` 同行
- `crates/acmecast-dns/src/providers/cloudflare.rs: cloudflare_credentials_schema` — `api_token.x-end-row`
- `crates/acmecast-dns/src/providers/aliyun.rs: aliyun_credentials_schema` — 密钥对同行，代理自然换行
- `crates/acmecast-dns/src/providers/tencent_eo.rs` — 复用 `tencent_credentials_schema`
- `crates/acmecast-core/src/proxy.rs: inject_proxy_field_annotations` — 代理下拉/联动/中文标签与 `x-end-row` 栅格判定单一来源
- `frontend/src/components/SchemaForm.tsx: orderedEntries / SchemaForm / ObjectFieldGroup` — `x-field-order` 定序与 `x-*` 渲染
## 凭据出网代理与全局代理配置

**全部凭据类型**（`cloud.tencent` / `dns.tencent-eo` / `cloud.cloudflare` / `cloud.aliyun` / `deploy.ssh` / `deploy.s3` / `deploy.webdav` / `acme.account`）支持凭据级代理模式（`proxy_mode` 下拉 + `proxy_url` 条件输入），全局代理经 `config.toml` 的 `[proxy]` 段或环境变量 `ACMECAST_PROXY_URL` 设一次，供凭据 `inherit` 继承（DNS 与部署链路同口径）与 DoH 代理回退共用。存量凭据缺字段视为 `direct`（直连），无需数据库迁移。

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

### 腾讯云 2 文件语义（对比声明）

`tencent-ssl` 部署目标保持 2 文件语义：上传参数 `CertificatePublicKey=chain_pem`（fullchain）与 `CertificatePrivateKey=key_pem`，不涉及文件化与 `ca.cer` 拆分。文档与校验口径 MUST 显式声明该差异，避免与文件类目标的 3 文件混淆——实现上 MUST NOT 引入 `ca.cer` 拆分逻辑。

### 字段与别名兼容

各目标新增 `fullchain`/`ca` 路径或模板字段，存量 `cert_path`/`cert_filename` 视为 `fullchain` 的别名（`fullchain_path`/`fullchain_filename` 未配置时生效）；`ca` 路径未显式配置时按 fullchain 同目录同前缀派生 `*.ca.cer`，**显式留空**可跳过 `ca.cer` 产出。打开存量流水线步骤时 `fullchain_path`/`fullchain_filename` 自动回显 `cert_path`/`cert_filename` 值。

| 目标 | 新增字段 | 别名字段 | 默认派生 |
|---|---|---|---|
| `local` | `fullchain_path` / `ca_path` | `cert_path` | `ca_path` ← fullchain 同目录 `*.ca.cer` |
| `ssh` | `fullchain_path` / `ca_path` | `cert_path` | `ca_path` ← fullchain 同目录 `*.ca.cer` |
| `s3` | `fullchain_filename` / `ca_filename` | `cert_filename` | `ca_filename` ← `<prefix>.ca.cer` |
| `webdav` | `fullchain_path` / `ca_path` | `cert_path` | `ca_path` ← `<prefix>.ca.cer` |

### 原子性、幂等与路径集合

- `local`/`ssh`：3 文件走原子写（临时文件 + `rename`）、`parse_mode`/`set_mode`/`set_owner` 校验；任一文件失败则整体失败，已写临时文件清理。
- `s3`/`webdav`：任一 PUT 失败即失败，不污染主存储；`DeployMode::SkipWrite`（指纹一致且非 `force`）时 3 文件路径均列入 `DeployOutcome.paths`，不发起写入。
- 运行历史与部署日志中引用的路径 MUST 仅包含实际部署的 3 文件（`*.ca.cer` / `*.fullchain.cer` / `*.key`），MUST NOT 引用不存在的叶子 `*.cer`。

### 实现索引

- `crates/acmecast-cert/src/pem.rs: split_chain / ca_pem_of` — 拆链纯函数（主存储与部署侧共用）
- `crates/acmecast-cert/src/lib.rs: domain_file_prefix` — 公共前缀派生（`*.example.com` → `_wildcard.example.com`）
- `crates/acmecast-deploy/src/targets/local.rs: LocalInput / derive_ca_path` — `fullchain_path`/`ca_path`/`key_path` 与别名回填、`ca_path` 派生
- `crates/acmecast-deploy/src/targets/ssh.rs: SshInput / ResolvedSshConfig / resolve_input` — 同 local 的别名回填与派生逻辑
- `crates/acmecast-deploy/src/targets/s3.rs: S3Input` — `fullchain_filename`/`ca_filename`/`key_filename` 与 `cert_filename` 别名
- `crates/acmecast-deploy/src/targets/webdav.rs: WebdavInput` — `fullchain_path`/`ca_path`/`key_path` 与 `cert_path` 别名
- `crates/acmecast-deploy/src/targets/tencent.rs` — 2 文件上传（`CertificatePublicKey=chain_pem` + `CertificatePrivateKey=key_pem`），不拆 `ca.cer`
- `docs/07-deploy-targets.md` — 各目标字段表与共享 3 文件语义

## SSH 部署跳板机（bastion / jump host）

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
