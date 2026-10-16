# credential-proxy Specification

## Purpose
TBD - created by archiving change credential-proxy-support. Update Purpose after archive.

## Requirements

### Requirement: 凭据 proxy_mode 字段语义与校验

全部凭据类型（`cloud.cloudflare` / `cloud.aliyun` / `cloud.tencent` / `dns.tencent-eo` / `deploy.ssh` / `deploy.s3` / `deploy.webdav` / `acme.account` / `notify.feishu` / `notify.dingtalk` / `notify.generic` / `notify.email`）MUST 暴露 `proxy_mode` 枚举（`direct` / `inherit` / `custom`）与可选 `proxy_url`，替代单一 `proxy_url` 字符串。`proxy_mode` schema MUST 注入 `enum_values: ["direct","inherit","custom"]` + `x-enum-labels` 下拉展示中文（直连 / 继承全局 / 自定义）；二字段 MUST 独占一行（`inject_proxy_field_annotations` 给 `proxy_mode` 前驱字段注入 `x-end-row`，使 `proxy_mode` 从新行首列开始与 `proxy_url` 同行：左下拉、右输入框，`proxy_url` 仅 `custom` 时显示于右侧）。`custom` 时 MUST 校验 scheme ∈ `{socks5, http, https}` 且可解析，非法值拒绝并定位到 `proxy_url`。存量兼容：旧 `proxy_url` 有值视为 `custom`、无值视为 `direct`。

#### Scenario: 代理模式下拉显示中文

- **WHEN** 打开任一凭据类型的编辑页该字段
- **THEN** 下拉选项显示"直连"、"继承全局"、"自定义"中文文案，而非裸 `direct`/`inherit`/`custom`

#### Scenario: 留空代理则直连

- **WHEN** 凭据 `proxy_mode=direct`（缺省）
- **THEN** 提供商 API 直连，行为与引入代理字段之前完全一致

#### Scenario: proxy_mode 与 proxy_url 独占一行

- **WHEN** 凭据编辑弹窗渲染（前驱字段由 `x-field-order` 确定）
- **THEN** `proxy_mode` 前驱字段带 `x-end-row`，`proxy_mode` 从新行首列开始，`proxy_mode`（左）与 `proxy_url`（右）同行

#### Scenario: custom 时同行显示输入框

- **WHEN** `proxy_mode=custom`
- **THEN** 同一行右侧显示 `proxy_url` 输入框；其他模式时右侧不显示输入框

#### Scenario: 非法代理 URL 在保存时被拒绝

- **WHEN** 保存凭据时 `proxy_mode=custom` 且 `proxy_url=ftp://127.0.0.1:21` 或 `not-a-url`
- **THEN** 保存被拒绝，错误定位到 `proxy_url` 字段，提示支持 `socks5://`、`http://`、`https://`

#### Scenario: 存量凭据缺字段视为直连

- **WHEN** 存量凭据 JSON 中不存在 `proxy_mode` 与 `proxy_url` 字段（变更前保存的数据）
- **THEN** 解析为 `direct`（直连），不报错、不要求迁移

#### Scenario: 存量凭据有旧 proxy_url 视为 custom

- **WHEN** 存量凭据 JSON 含 `proxy_url=socks5://127.0.0.1:7890` 但无 `proxy_mode`
- **THEN** 视为 `proxy_mode=custom`+该值，行为不变

#### Scenario: 通知凭据同样暴露代理字段

- **WHEN** 打开 `notify.feishu` / `notify.dingtalk` / `notify.generic` / `notify.email` 任一类型的凭据编辑弹窗
- **THEN** 表单含 `proxy_mode` 下拉（中文选项）与 `proxy_url` 输入框，布局与 DNS/部署凭据一致；`proxy_mode=custom` 且 `proxy_url` 非法时保存被拒绝并定位到 `proxy_url`

### Requirement: 出网请求经凭据代理发送

配置了代理（`proxy_mode=inherit` 且全局非空，或 `proxy_mode=custom`）的凭据，其对应出网行为 MUST 经该代理发送，且 MUST NOT 改变其他凭据的出网路径：

- `cloud.cloudflare` / `cloud.aliyun` / `cloud.tencent` / `dns.tencent-eo`：DNS 挑战查询/创建/删除与凭据连通性测试（HTTP）。
- `deploy.webdav`：MKCOL/PUT 探测与部署上传（HTTP）。
- `deploy.s3`：`PutObject`/`DeleteObject` 探测与部署上传（S3 HTTP API）。
- `acme.account`：ACME 目录/下单/校验等（HTTPS，经 `acmecast_acme::ProxyConfig`）。
- `deploy.ssh`：SSH 连接与主机探测（TCP，经 SOCKS5/HTTP-CONNECT 隧道）。
- `notify.feishu` / `notify.dingtalk` / `notify.generic`：运行结束简讯投递与凭据连通性测试（HTTP，经 `reqwest::Proxy`，支持 `socks5`/`http`/`https`）。
- `notify.email`：运行结束简讯投递与凭据连通性测试（SMTP，经 SOCKS5/HTTP-CONNECT 隧道后完成 SMTP 会话；支持 `socks5`/`http`/`https` 三种代理协议）。

代理连接失败 MUST 以可读错误呈现（HTTP 类为 `provider_api_error`，SSH 类为连接错误，通知类为投递错误）且含代理主机以便排障，错误与日志中经 `redact` 脱敏（`socks5://user:pass@host:port` 不明文外泄）。各类型的连通性测试 MUST 走与正式调用相同的代理。

#### Scenario: HTTP 类凭据配置代理后出网经代理

- **WHEN** WebDAV 凭据 `proxy_mode=inherit` 且全局 `ACMECAST_PROXY_URL=socks5://127.0.0.1:7890`，执行部署上传
- **THEN** 上传请求经该 socks5 代理发送；代理不可达时错误信息含代理主机

#### Scenario: 配置 socks5 代理后 DNS 挑战经代理发送

- **WHEN** Cloudflare 凭据 `proxy_mode=inherit` 且全局 `ACMECAST_PROXY_URL=socks5://127.0.0.1:7890`，流水线以 DNS-01 挑战调用 Cloudflare API 查询/写入 TXT 记录
- **THEN** 出网请求经该 socks5 代理发送，挑战正常完成；代理不可达时报 `provider_api_error` 且错误信息含代理主机

#### Scenario: ACME 账号凭据代理生效

- **WHEN** `acme.account` 凭据 `proxy_mode=custom` 且 `proxy_url=http://10.0.0.1:3128`，执行证书申请
- **THEN** ACME 出网经该 HTTP 代理发送

#### Scenario: SSH 主机凭据代理生效

- **WHEN** `deploy.ssh` 凭据 `proxy_mode=custom` 且 `proxy_url=socks5://127.0.0.1:7890`，执行部署连接
- **THEN** SSH 连接先建立 SOCKS5 隧道再握手，主机探测同样经该隧道

#### Scenario: 通知 webhook 凭据配置代理后投递经代理

- **WHEN** `notify.generic` 凭据 `proxy_mode=custom` 且 `proxy_url=socks5://127.0.0.1:7890`，流水线运行结束向该渠道投递简讯
- **THEN** 投递请求经该 socks5 代理发送；代理不可达时 WARN 日志与错误信息含代理主机（剥 userinfo）

#### Scenario: 邮件凭据配置代理后 SMTP 经代理

- **WHEN** `notify.email` 凭据 `proxy_mode=inherit` 且全局 `ACMECAST_PROXY_URL=socks5://127.0.0.1:7890`，流水线运行结束向该渠道投递邮件
- **THEN** SMTP 连接先建立 SOCKS5 隧道再完成 EHLO/TLS/认证/投递；代理不可达时错误信息含代理主机

#### Scenario: 通知凭据 inherit 继承全局代理

- **WHEN** `notify.feishu` 凭据 `proxy_mode=inherit` 且全局 `ACMECAST_PROXY_URL` 已配，运行结束投递简讯
- **THEN** 投递经全局代理出网，行为与 `proxy_mode=custom` 配同 URL 一致

#### Scenario: 代理不泄漏到其他出网路径

- **WHEN** 某凭据 `proxy_mode=inherit`，同一流水线中其他步骤使用 `proxy_mode=direct` 的凭据
- **THEN** 仅前者经代理，其余出网请求路径不变

#### Scenario: 连通性测试走代理

- **WHEN** 凭据 `proxy_mode=inherit` 且全局代理已配，在凭据页执行连通性测试，而代理进程未启动
- **THEN** 测试失败且错误信息指向代理地址，而非呈现为凭据无效

### Requirement: 凭据代理出网操作超时

凭据配置了代理（`proxy_mode=custom` 或 `proxy_mode=inherit` 且全局非空）时，经代理出网的网络操作 MUST 在配置的超时时长（默认 30 秒，由 `[network] timeout_secs` / `ACMECAST_NETWORK_TIMEOUT_SECS` 控制）内完成或以超时错误失败，绝不永久挂起。`proxy_mode=inherit` 对 DNS 与部署（`deploy.ssh`/`deploy.s3`/`deploy.webdav`/`tencent-ssl`）两条链路均继承全局代理（此前部署链路 `inherit` 等同直连的限制移除）。超时计时从代理隧道 TCP 流建立完成后开始——代理 TCP 连接阶段已有操作系统级 TCP 超时兜底，不计入超时时长。直连路径同样施加超时，口径与代理路径一致。该超时约束覆盖 DNS 提供商 API 调用、SSH 部署连接与文件传输、S3/WebDAV 上传、腾讯云 SSL API 调用。超时错误 MUST 在错误信息中体现代理主机或目标主机以助排障。通知投递（webhook 与 SMTP）经代理时 MUST 在投递超时（`acmecast_notify::delivery::DEFAULT_TIMEOUT`，10 秒）内完成或失败——SMTP 的隧道建立与会话整体计入该超时。

#### Scenario: 经代理 SSH 部署超时

- **WHEN** SSH 凭据 `proxy_mode=custom`，代理 TCP 连接成功但隧道建立后 SSH 握手无响应，超时配置为默认 30 秒
- **THEN** 30 秒后部署步骤以超时错误失败，超时从代理 TCP 连接成功后开始，错误信息含代理主机

#### Scenario: 经代理 DNS API 调用超时

- **WHEN** DNS 凭据 `proxy_mode=inherit` 且全局代理已配，代理 TCP 连接成功但 API 请求无响应
- **THEN** 超时时长后操作以超时错误失败，超时从 HTTP 请求发送后开始

#### Scenario: 直连路径同样受超时约束

- **WHEN** 凭据 `proxy_mode=direct`，目标端点不可达且无响应
- **THEN** 超时时长后操作以超时错误失败，口径与代理路径一致

#### Scenario: 代理 TCP 连接阶段不计入超时时长

- **WHEN** 凭据配置了代理，代理主机 TCP 连接本身耗时 10 秒后成功，随后隧道内操作在 20 秒内完成（默认超时 30 秒）
- **THEN** 操作正常成功（30 秒从代理 TCP 连接成功后开始计算，20 < 30）

#### Scenario: 自定义超时时长经代理生效

- **WHEN** `ACMECAST_NETWORK_TIMEOUT_SECS=60`，代理 TCP 连接成功后 SSH 握手在 40 秒完成
- **THEN** 操作正常成功（40 < 60）

#### Scenario: 部署 inherit 继承全局代理

- **WHEN** 部署凭据 `proxy_mode=inherit` 且全局 `ACMECAST_PROXY_URL` 已配（如 `socks5://127.0.0.1:40027`），部署目标为 `deploy.ssh`/`deploy.s3`/`deploy.webdav`/`tencent-ssl` 之一
- **THEN** 该部署操作经全局代理出网，行为与 `proxy_mode=custom` 配同 URL 一致；超时仍从隧道建立后计算

#### Scenario: 经代理通知投递超时

- **WHEN** 通知凭据配置了代理，代理不可达且连接挂起（无响应）
- **THEN** 投递在 10 秒内以超时错误失败（webhook 经客户端超时，SMTP 经隧道建立与会话整体超时），错误信息含代理主机，运行结果不受影响
