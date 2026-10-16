# Spec Delta

## MODIFIED Requirements

### Requirement: 通知凭据类型展示名与表单布局

凭据新建/编辑弹窗的类型下拉 MUST 包含四种通知凭据类型，展示名分别为：`notify.feishu → 飞书机器人`、`notify.dingtalk → 钉钉机器人`、`notify.generic → 通用 Webhook`、`notify.email → 邮件`。通知凭据的字段表单 MUST 按 schema 扩展组织布局，复用现有 `x-full-width`/`x-end-row`/`x-multiline` 约定。通知凭据表单 MUST 含 `proxy_mode` 枚举下拉（`direct`/`inherit`/`custom`，中文展示为直连/继承全局/自定义）与 `proxy_url` 输入框：两字段独占一行（左下拉、右输入框），`proxy_url` 仅 `proxy_mode=custom` 时显示——布局由 `inject_proxy_field_annotations` 注入，与 DNS/部署凭据完全一致。

#### Scenario: 类型下拉包含通知凭据

- **WHEN** 用户打开凭据新建弹窗并展开类型下拉
- **THEN** 下拉选项包含「飞书机器人」「钉钉机器人」「通用 Webhook」「邮件」四项

#### Scenario: 飞书凭据字段布局

- **WHEN** 用户选择 `notify.feishu` 类型
- **THEN** 表单展示 `url`（仅一行，`x-full-width`）、`secret`（左半列独占一行，`x-end-row`，可选不填即无签名），`url` 与 `secret` 不再与 `name` 同行

#### Scenario: 通用 Webhook 凭据字段布局

- **WHEN** 用户选择 `notify.generic` 类型
- **THEN** 表单展示 `url`（仅一行）、`method`（左半列独占一行，枚举下拉）、`headers`（仅一行多行文本）、`body_template`（仅一行多行文本）

#### Scenario: 邮件凭据字段布局

- **WHEN** 用户选择 `notify.email` 类型
- **THEN** 表单展示 `host`（左）/`port`（右）同行、`from`（左）/`tls`（右，枚举下拉）同行、`to`（仅一行多行文本，`x-full-width`）、`username`（左）/`password`（右）同行

#### Scenario: 通知凭据的代理字段独占一行

- **WHEN** 用户选择任一 `notify.*` 类型，展开到表单末尾的代理字段
- **THEN** `proxy_mode` 从新行首列开始（其前驱字段带 `x-end-row`），与 `proxy_url` 同行：左下拉显示「直连/继承全局/自定义」，右侧输入框仅选择「自定义」时显示

#### Scenario: 通知凭据代理下拉显示中文

- **WHEN** 用户展开 `notify.*` 凭据的 `proxy_mode` 下拉
- **THEN** 选项显示「直连」「继承全局」「自定义」，而非裸 `direct`/`inherit`/`custom`
