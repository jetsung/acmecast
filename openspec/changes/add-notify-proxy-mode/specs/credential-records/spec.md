# Spec Delta

## MODIFIED Requirements

### Requirement: 通知凭据类型的凭据记录

系统 MUST 支持四种通知凭据类型的凭据记录创建与持久化：`notify.feishu`、`notify.dingtalk`、`notify.generic`、`notify.email`。凭据记录的 `type_id` MUST 为上述标识之一，`encrypted_fields` MUST 以 `CredentialCipher` 加密存储渠道配置 JSON（含 `proxy_mode` / `proxy_url` 代理配置）。通知凭据类型的 `capabilities` MUST 为空数组（非受控类型）。凭据列表 API MUST 在按 `type_id` 过滤时支持通知类凭据。

#### Scenario: 创建飞书渠道凭据

- **WHEN** 用户通过凭据创建 API 提交 `type_id = "notify.feishu"`、`name = "生产飞书群"`、`fields = {"url": "https://open.feishu.cn/...", "secret": "xxx"}`
- **THEN** 凭据加密落库，列表 API 返回该记录，`capabilities` 为空数组，`encrypted_fields` 不以明文返回

#### Scenario: 凭据列表按通知类型过滤

- **WHEN** 用户请求凭据列表并指定 `type_id = "notify.email"`
- **THEN** 返回所有 `type_id` 为 `notify.email` 的凭据记录，不含其他类型

#### Scenario: 通知凭据可被流水线的通知字段引用

- **WHEN** 流水线定义的 `notification_channels` 含凭据 ID `7`，且凭据 7 的 `type_id` 为 `notify.feishu`
- **THEN** 运行结束投递简讯时 `CredentialStore::resolve(7)` 成功返回解密后的飞书渠道配置（含代理配置）

#### Scenario: 通知凭据记录的代理配置加密落库

- **WHEN** 用户提交 `type_id = "notify.generic"`、`name = "通用渠道"` 且 `fields = {"url": "https://ops.example.com/hook", "proxy_mode": "custom", "proxy_url": "socks5://127.0.0.1:7890"}`
- **THEN** 凭据的 `name` 为 `通用渠道`，代理配置随 `encrypted_fields` 一并加密落库，读取时原样回显、不以明文暴露于列表响应
