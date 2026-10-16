# Spec Delta

## MODIFIED Requirements

### Requirement: 通知渠道凭据类型

系统 MUST 支持四种通知凭据类型，作为 `CredentialType` trait 的实现注册到凭据注册表：`notify.feishu`（飞书/Lark 自定义机器人，字段含 `url`、`secret`、`proxy_mode`、`proxy_url`）、`notify.dingtalk`（钉钉自定义机器人，字段含 `url`、`secret`、`proxy_mode`、`proxy_url`）、`notify.generic`（通用 webhook，字段含 `url`、`method`、`headers`、`body_template`、`proxy_mode`、`proxy_url`）、`notify.email`（邮件，字段含 `host`、`port`、`tls`、`from`、`to`、`username`、`password`、`proxy_mode`、`proxy_url`）。`proxy_mode` / `proxy_url` 的语义、schema 注入（枚举下拉、中文标签、联动显隐、独占一行）与保存校验 MUST 与其余凭据类型共用同一套共享原语（`acmecast_core::proxy`）。凭据展示名取记录外层 `name`，字段 JSON 中不再含 `name`。凭据字段 MUST 经 `CredentialCipher` 加密落库。通知凭据类型 MUST 不受 `capabilities` 能力标记约束（非受控类型）。

#### Scenario: 飞书渠道凭据可创建并加密落库

- **WHEN** 用户在凭据页创建一条 `notify.feishu` 类型凭据，填写 `url`、`secret`
- **THEN** 凭据加密落库，列表中可见该凭据，字段值不以明文返回

#### Scenario: 通用 webhook 凭据可创建

- **WHEN** 用户在凭据页创建一条 `notify.generic` 类型凭据，填写 `url`、`method`、`headers`、`body_template`
- **THEN** 凭据加密落库，列表中可见该凭据

#### Scenario: 通知凭据不出现在流水线步骤凭据下拉

- **WHEN** 用户在流水线编辑页的 `cert.apply` 步骤打开 `dns_credential_id` 下拉
- **THEN** 选项中不含 `notify.feishu`、`notify.dingtalk`、`notify.generic`、`notify.email` 类型的凭据

#### Scenario: 通知凭据可配置代理模式

- **WHEN** 用户创建一条 `notify.generic` 凭据并选择 `proxy_mode=custom`、`proxy_url=socks5://127.0.0.1:7890`
- **THEN** 凭据保存成功，代理配置随字段 JSON 加密落库；`proxy_mode=custom` 且 `proxy_url` 非法时保存被拒绝并定位到 `proxy_url`

### Requirement: 通知渠道连通性测试

四种通知凭据类型 MUST 支持 `CredentialType::test_connectivity`：`notify.feishu` 与 `notify.dingtalk` 向机器人 webhook 地址发送一条测试文本消息；`notify.generic` 向配置的 URL 发送一条测试 JSON 负载；`notify.email` 向 `to` 列表发送一封测试邮件（主题含「测试」标识）。连通性测试 MUST 走与正式投递相同的出网路径：webhook 三类经凭据 `proxy_mode` 决定的代理（`reqwest::Proxy`），邮件经凭据 `proxy_mode` 决定的代理隧道完成 SMTP 会话。测试结果 MUST 经脱敏后呈现；代理不可达时失败原因 MUST 含代理主机（剥 userinfo）。

#### Scenario: 飞书渠道连通性测试成功

- **WHEN** 用户对一条 `notify.feishu` 凭据调用连通性测试
- **THEN** 飞书机器人收到一条测试消息，测试结果为成功

#### Scenario: 邮件渠道连通性测试成功

- **WHEN** 用户对一条 `notify.email` 凭据调用连通性测试
- **THEN** `to` 列表中的收件人收到一封测试邮件，测试结果为成功

#### Scenario: 连通性测试经凭据代理出网

- **WHEN** `notify.generic` 凭据 `proxy_mode=custom` 且 `proxy_url=socks5://user:secret@127.0.0.1:1`（不可达），调用连通性测试
- **THEN** 测试失败，原因含代理主机 `127.0.0.1:1` 且不含 `secret`（经脱敏）

#### Scenario: 邮件连通性测试经代理隧道

- **WHEN** `notify.email` 凭据 `proxy_mode=inherit` 且全局代理已配，调用连通性测试
- **THEN** SMTP 会话经该代理隧道完成，测试结果为成功；代理不可达时原因含代理主机
