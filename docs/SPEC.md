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
- `crates/acmecast-server/src/steps/provider_select.rs` — `dns_provider`/`dns_credential_id` 全空时自动选择
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

