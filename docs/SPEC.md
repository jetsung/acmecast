# AcmeCast 项目规范（SPEC）

本文件为全局规范，涵盖 ACME 账号、DNS 校验与证书生命周期等产品行为，`docs/SPEC.md` 为唯一真实来源。下文首节为 DNS Zone 推导，其余为内建 CA 等全局约定。

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

`cert.apply` 的 DNS-01 流程以 `dns_credential_id` 为唯一凭据参数，不再接受 `dns_provider` 输入字段——提供商标识由凭据记录的 `type_id` 直接推导（如 `cloudflare` 类型凭据 → Cloudflare 提供商）。已有流水线配置中残留的 `dns_provider` 值在加载时被静默忽略，行为以凭据 `type_id` 为准。

### 显式指定凭据

`dns_credential_id` 非空时，系统从 `CredentialStore::resolve()` 返回的凭据 `type_id` 推导提供商标识，以此提供商完成 TXT 记录的写入、传播等待的解析器排序与记录清理。

### 自动选择（`dns_credential_id` 为空）

未指定凭据时，系统遍历凭据库中所有 DNS 类凭据（凭据 `type_id` 即候选标识），逐个探测其能否管理目标 zone，取第一个探测通过的（凭据，提供商）组合。候选遍历顺序为：按凭据 `type_id` 的注册表稳定序，同一类型多条凭据按 ID 升序。每个失败候选的原因经步骤日志输出；全部失败时步骤报错，错误中汇总各候选原因。

### DoH 解析器排序

传播等待阶段用于判定"CA 能否看到 TXT"的公共解析器集合按提供商标识感知排序：首选提供商对应的同厂商 DoH 端点排第一，其后按固定序排列其余厂商 DoH 作为兜底。排序规则：

| 提供商 | 首选 DoH | 兜底序 |
|---|---|---|
| `cloudflare` | Cloudflare | Cloudflare → 阿里 → 腾讯 → Google |
| `aliyun` | 阿里 | 阿里 → Cloudflare → 腾讯 → Google |
| `tencent` / `tencent-eo` | 腾讯 | 腾讯 → Cloudflare → 阿里 → Google |
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

仅 `tencent`、`cloudflare`、`aliyun` 三类凭据支持能力标记；其他类型（`tencent-eo`、`ssh`、`s3`、`webdav`、`acme.account` 等）不暴露、不持久化、不返回该字段。

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
| `credential_id`（`cert.deploy` 的 `tencent-ssl` 变体） | 仅列 `type_id == "tencent"` 且 `capabilities` 含 `deploy` 的凭据 |

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
| `tencent` | 腾讯云 |
| `tencent-eo` | 腾讯云 EdgeOne |
| `cloudflare` | Cloudflare |
| `aliyun` | 阿里云 |
| `ssh` | SSH 主机 |
| `s3` | S3 兼容存储 |
| `webdav` | WebDAV |
| `acme.account` | ACME 账号 |

`type_id` 不变，流水线与存储不受影响。依赖 `display_name` 做精确匹配的外部脚本需更新。

### 实现索引

- `crates/acmecast-server/src/credentials.rs` — `display_name()` 返回短名称
- `crates/acmecast-server/src/handlers.rs: list_credential_types` — 透传 `display_name`
- `frontend/src/pages/credentials/CredentialListPage.tsx` — 类型下拉 `options[].label = display_name`

## 凭据字段表单布局约定

凭据字段表单由后端 schema（`schemars::JsonSchema` + `x-*` 扩展）驱动，前端 `SchemaForm` 为通用渲染引擎。默认标量字段半宽（`span 12`）两列一行；带 `x-full-width` 的字段独占整行；带 `x-end-row` 的半宽字段行尾补空占位列换行；`x-field-order` 钉字段渲染顺序，对抗 `serde_json::Map` 按字母序重排 `properties`。

### S3 兼容存储凭据字段分组

`S3 兼容存储`（`type_id: s3`）凭据字段按业务语义分组为三行、每行两列，顺序固定为 `access_key` → `secret_key` → `endpoint` → `bucket` → `region` → `prefix`，`session_token` 置于末尾。

| 行 | 字段 |
|---|---|
| 第一行 | `access_key` ｜ `secret_key`（密钥对） |
| 第二行 | `endpoint` ｜ `bucket`（接入与桶） |
| 第三行 | `region` ｜ `prefix`（区域与前缀） |
| 末尾 | `session_token`（半宽） |

三行均为未标注的标量半宽字段、两两一行的默认流式布局（`span 12`），不注入 `x-full-width` 或 `x-end-row`；`session_token` 保持默认半宽、排在所有分组之后，不影响前三行成对布局。

### 实现索引

- `crates/acmecast-access/src/credential/s3.rs: s3_fields_schema` — 注入 `x-field-order: ["access_key","secret_key","endpoint","bucket","region","prefix","session_token"]`
- `crates/acmecast-server/src/credentials.rs: tencent_credentials_schema` — 参考既有注入模式
- `frontend/src/components/SchemaForm.tsx: orderedEntries` — `x-field-order` 定序渲染

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
