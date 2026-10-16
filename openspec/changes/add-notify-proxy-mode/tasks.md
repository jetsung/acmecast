# Tasks

## 1. 通知凭据字段与 schema

- [x] 1.1 在 `crates/acmecast-notify/src/notify_credential.rs` 中给 `FeishuFields`/`DingtalkFields`/`GenericFields`/`EmailFields` 各增加 `proxy_mode: Option<String>` 与 `proxy_url: Option<String>` 字段（`serde(default, skip_serializing_if = "Option::is_none")`，文档注释与既有凭据类型同文案）。验证：`cargo build -p acmecast-notify` 通过
- [x] 1.2 四个 `*_fields_schema()` 的 `x-field-order` 在末尾追加 `"proxy_mode","proxy_url"`，并在函数末尾调用 `acmecast_core::inject_proxy_field_annotations(&mut schema)`。验证：`cargo build -p acmecast-notify` 通过；单测断言四个 schema 渲染含 `"enum":["direct","inherit","custom"]`、中文标签与 `proxy_url` 的 `x-visible-when`
- [x] 1.3 四个 `CredentialType::validate` 在既有校验后追加 `acmecast_core::validate_proxy_field(fields)?`。验证：单测断言 `proxy_mode=custom` 且 `proxy_url` 为空/非法 scheme 时保存被拒且错误定位到 `proxy_url`；`direct`/`inherit` 放行

## 2. 有效代理映射与 ChannelConfig

- [x] 2.1 在 `crates/acmecast-notify/src/channel.rs` 的 `ChannelConfig` 增加 `proxy_url: Option<String>` 字段（有效代理 URL，`None` 即直连）。同步更新全部构造点（`notify_credential.rs` 四处映射、`delivery.rs`/`email.rs`/`sink.rs` 测试构造）。验证：`cargo build -p acmecast-notify` 通过
- [x] 2.2 `channel_config_from_parts` 增加 `global_proxy_url: Option<&str>` 参数，构造时以 `acmecast_core::proxy::effective_proxy_url(fields, global_proxy_url)` 填充 `proxy_url`；`channel_config_from_credential` 透传该参数。验证：单测覆盖 direct→`None`、inherit→全局值、custom→凭据值、缺省→`None`
- [x] 2.3 `smtp_settings_from` 把有效代理 URL 写入 `SmtpSettings.proxy_url`（`SmtpSettings` 同步增加该字段）。验证：`cargo build -p acmecast-notify` 通过

## 3. webhook 投递经代理

- [x] 3.1 在 `crates/acmecast-notify/src/delivery.rs` 新增 `http_client_with_proxy(proxy_url: Option<&str>) -> Result<reqwest::Client>`：`reqwest::Proxy::all(url)` 注入；`deliver` 在渠道 `proxy_url` 为 `Some` 时用它构建客户端，否则沿用传入的共享客户端。验证：单测断言 `socks5://`/`http://` 客户端可构建、非法值返回错误
- [x] 3.2 `notify_credential.rs` 的 `probe()` 改为按 `effective_proxy_url(fields, self.global_proxy_url.as_deref())` 经 `http_client_with_proxy` 取客户端；四个 `CredentialType` 由 unit struct 改为持有 `global_proxy_url: Option<String>` 字段（`#[derive(Debug, Default)]`）+ `with_global_proxy` 构造器。验证：`cargo build -p acmecast-notify` 通过
- [x] 3.3 代理连接失败的连通性测试结果 MUST 含代理主机（剥 userinfo）——`probe` 的错误原因经 `proxy_log_target` 口径呈现，且经 `redact` 脱敏。验证：单测以不可达代理（`socks5://user:secret@127.0.0.1:1`）断言原因含 `127.0.0.1:1` 且不含 `secret`

## 4. 邮件 SMTP 经代理隧道

- [x] 4.1 新增 `crates/acmecast-notify/src/smtp_proxy.rs`：实现隧道建立（`socks5://` 经 `tokio-socks`；`http://` 经 CONNECT + `Proxy-Authorization: Basic`；`https://` 经「TLS 到代理 + CONNECT」）与 `AsyncTokioStream` 包装结构。验证：`cargo build -p acmecast-notify` 通过
- [x] 4.2 实现经代理的 SMTP 会话：`AsyncSmtpConnection::connect_with_transport` →（`starttls`）→（`auth`）→ `send`；隐式 TLS 先用 `tokio-rustls`（root 取 `webpki-roots`）在隧道流上包装。`EmailChannel::send` 在 `SmtpSettings.proxy_url` 为 `Some` 时改走该路径，无代理保持现有 `build_transport` 路径。验证：`cargo build -p acmecast-notify` 通过
- [x] 4.3 隧道建立 + 会话整体包在 `tokio::time::timeout(delivery::DEFAULT_TIMEOUT)` 内；错误信息含代理主机（`proxy_log_target`）。验证：单测用本地假代理（未启动/拒绝连接）断言超时或错误含代理地址
- [x] 4.4 在 workspace `Cargo.toml` 与 `crates/acmecast-notify/Cargo.toml` 把 `tokio-socks`、`tokio-rustls`、`webpki-roots` 提升为直接依赖（版本与依赖树中既有版本一致：`tokio-socks 0.5`、`tokio-rustls 0.26`）。验证：`cargo build -p acmecast-notify` 通过，`Cargo.lock` 无新增版本条目

## 5. 全局代理装配

- [x] 5.1 `WebhookEventSink` 增加 `global_proxy_url: Option<String>` 字段；`new`/`with_registry` 增加对应参数，`resolve_channels` 传给 `channel_config_from_credential`。验证：`cargo build -p acmecast-notify` 通过
- [x] 5.2 `acmecast-server/src/main.rs` 装配 `WebhookEventSink` 时传 `config.proxy_url.as_deref()`；`handlers.rs::default_credential_registry` 中四个通知凭据类型改为 `::default().with_global_proxy(proxy_url)` 注册。验证：`cargo build -p acmecast-server` 通过
- [x] 5.3 `sink.rs` 测试更新 `WebhookEventSink::new` 调用并新增 `inherit` 场景：全局代理注入后 `resolve_channels` 产出的渠道 `proxy_url` 等于全局值。验证：`cargo test -p acmecast-notify` 通过

## 6. 测试

- [x] 6.1 `acmecast-notify/tests/notify_credential.rs` 扩展：四种类型 schema 的代理注入断言（枚举/标签/联动/`x-end-row`）、`proxy_mode=custom` 校验失败定位 `proxy_url`、`channel_config_from_credential` 的代理计算（direct/inherit/custom）。验证：`cargo test -p acmecast-notify -- notify_credential` 通过
- [x] 6.2 新增 SMTP 隧道测试：本地假 HTTP CONNECT 代理（接收 CONNECT 后建立 TCP 隧道转发到假 SMTP 服务）验证邮件经代理送达；假 SOCKS5 代理同法。验证：`cargo test -p acmecast-notify -- smtp_proxy` 通过
- [x] 6.3 `acmecast-server` 侧测试更新：`notifications_e2e.rs` 中 `WebhookEventSink::new` 调用补参数；`credentials.rs` 单测覆盖通知凭据类型注册后 `with_global_proxy` 生效。验证：`cargo test -p acmecast-server -- credentials` 通过
- [x] 6.4 全量回归：`cargo test -p acmecast-notify`、`cargo test -p acmecast-core`、`cargo test -p acmecast-server`（对照 memory 中记录的既有失败清单，确认无新增回归）；`cargo clippy -p acmecast-notify -p acmecast-server --all-targets` 无新增告警。验证：命令全部通过（既有失败除外）

## 7. 文档

- [x] 7.1 `docs/04-credentials.md`：`notify.*` 四类字段表增加 `proxy_mode`/`proxy_url` 行；删除「通知凭据本身也不含代理字段」表述；补代理三态说明与示例。验证：文档与实现一致
- [x] 7.2 `docs/03-configuration.md`：「通知投递不接代理」改为按凭据代理配置出网；`docs/SPEC.md` 代理条目同步（通知投递的出网路径按凭据代理模式）。验证：全文无残留「通知投递不接代理」表述

## 8. 端到端冒烟（手动）

- [x] 8.1 启动开发后端，创建一条 `notify.generic` 凭据配 `proxy_mode=custom` + 本地不可达代理，连通性测试报错含代理主机；改为 `direct` 后连通性测试成功（对照本地接收器）。验证：两次测试结果符合预期
- [x] 8.2 配 `proxy_mode=inherit` + 全局 `ACMECAST_PROXY_URL` 指向本地可用代理（如 `http://127.0.0.1:8888` 假 CONNECT 代理），运行流水线至结束，验证简讯经代理投递（假代理日志可见 CONNECT 记录）。验证：投递成功且代理日志可见
