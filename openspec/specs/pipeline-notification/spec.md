# pipeline-notification Specification

## Purpose
定义流水线级通知渠道选择与统一简讯投递行为：在流水线模型上新增 `notification_channels` 字段（通知类凭据 ID 数组），运行结束（成功或失败）时组装一条简讯并按选定渠道投递，使管理员无需逐条查看逐步骤推送即可感知整条流水线运行结果。

## Requirements

### Requirement: 流水线模型新增通知渠道字段

流水线模型 MUST 新增 `notification_channels` 字段——整数数组，元素为通知类凭据（`type_id` 为 `notify.feishu`/`notify.dingtalk`/`notify.generic`/`notify.email`）的凭据 ID。字段缺省为空数组。创建与更新流水线的 API MUST 透传该字段，响应中原样回显。`notification_channels` 引用了不存在或非通知类凭据的 ID 时 MUST 以校验错误失败，错误信息定位到 `notification_channels` 字段并指明无效 ID。

#### Scenario: 创建流水线时选择通知渠道

- **WHEN** 用户创建流水线，提交 `notification_channels = [1, 3]`（凭据 1 为 `notify.feishu`、凭据 3 为 `notify.email`）
- **THEN** 流水线保存成功，响应中 `notification_channels = [1, 3]` 原样回显

#### Scenario: 未选择通知渠道时保存空数组

- **WHEN** 用户创建流水线时未选择通知渠道，提交 `notification_channels = []` 或缺省
- **THEN** 流水线保存成功，`notification_channels` 为空数组

#### Scenario: 引用非通知类凭据 ID 时校验失败

- **WHEN** 用户提交 `notification_channels = [5]`，凭据 5 的 `type_id` 为 `deploy.ssh`
- **THEN** 请求以校验错误失败，错误信息含凭据 ID `5` 并定位到 `notification_channels` 字段

### Requirement: 简讯组装内容

运行结束投递的简讯 MUST 为文本格式，至少包含：流水线名称、触发来源（手动/定时/续期）、运行结果（成功/失败）、各步骤的执行状态与标识（步骤类型 + 成功/失败/跳过）、运行总耗时。`cert.apply` 步骤成功时简讯 MUST 包含本次申请的域名列表。失败简讯 MUST 包含失败步骤的 `type_id` 与失败原因。简讯 MUST NOT 包含私钥等敏感凭据内容。

#### Scenario: 成功运行简讯字段完整

- **WHEN** 一条含 `cert.apply` 与 `cert.deploy` 的流水线全部成功完成，`notification_channels = [1]`
- **THEN** 渠道 1 收到一条成功简讯，可读出流水线名称、触发来源、运行结果为成功、各步骤类型与状态、总耗时，且 `cert.apply` 对应行含域名列表，不含私钥明文

#### Scenario: 失败简讯含失败步骤与原因

- **WHEN** 流水线第 2 步失败，`notification_channels = [1]`
- **THEN** 渠道 1 收到一条失败简讯，可读出运行结果为失败、失败步骤类型标识与失败原因摘要，不含私钥明文

#### Scenario: 含跳过步骤的简讯

- **WHEN** 流水线中某步骤被停用跳过，运行结束后投递简讯
- **THEN** 简讯中该步骤状态标注为「跳过」，其余步骤正常列出

### Requirement: 运行结束统一投递

系统 MUST 在流水线运行结束（`Succeeded` 或 `Failed` 事件）时统一投递一条简讯。成功时投递成功简讯，失败时投递失败简讯。投递渠道为流水线定义的 `notification_channels` 凭据 ID 列表，运行时通过 `CredentialStore::resolve(id)` 取解密后的渠道配置。投递 MUST 复用 `acmecast_notify::deliver`（含超时、退避重试与可观测日志）。投递出网路径 MUST 按渠道凭据的 `proxy_mode` 决定：`direct` 直连、`inherit` 继承全局代理（`[proxy] url` / `ACMECAST_PROXY_URL`）、`custom` 走凭据自带 `proxy_url`——webhook 三类经 `reqwest::Proxy`，邮件经代理隧道完成 SMTP 会话。投递成败 MUST NOT 改变流水线运行结果与运行历史状态；投递失败时 MUST 输出 WARN 日志记录失败的渠道名与原因（代理不可达时含代理主机）。`notification_channels` 为空时 MUST 不产生任何投递。MUST NOT 存在逐步骤推送——不再按 `events` 列表逐步骤过滤投递。

#### Scenario: 成功时只收到一条简讯

- **WHEN** 一条含 `cert.apply` 与 `cert.deploy` 的流水线全部成功，`notification_channels = [1]`
- **THEN** 渠道 1 只收到一条成功简讯（而非 `cert.apply` 与 `cert.deploy` 各一条）

#### Scenario: 失败时只收到一条失败简讯

- **WHEN** 流水线第 2 步失败导致运行中止，`notification_channels = [1]`
- **THEN** 渠道 1 只收到一条失败简讯，含失败步骤标识与原因

#### Scenario: 无通知渠道时不投递

- **WHEN** 流水线 `notification_channels = []`，运行结束
- **THEN** 不产生任何通知投递

#### Scenario: 投递失败不影响运行结果

- **WHEN** 流水线运行成功，但某渠道投递最终失败（端点不可达或超时）
- **THEN** 运行历史状态仍为成功，WARN 日志记录失败的渠道名与原因

#### Scenario: 投递按渠道凭据的代理配置出网

- **WHEN** `notification_channels = [1, 2]`，凭据 1 为 `notify.generic`（`proxy_mode=custom`，`proxy_url=socks5://127.0.0.1:7890`），凭据 2 为 `notify.feishu`（`proxy_mode=direct`），流水线运行结束
- **THEN** 凭据 1 的投递经 socks5 代理发送，凭据 2 直连发送——各渠道互不影响

#### Scenario: inherit 继承全局代理投递

- **WHEN** 渠道凭据 `proxy_mode=inherit` 且全局 `ACMECAST_PROXY_URL` 已配，流水线运行结束
- **THEN** 该渠道的简讯经全局代理出网投递；全局未配时直连，投递照常

#### Scenario: 代理不可达时投递失败但不影响运行

- **WHEN** 渠道凭据配置的代理不可达，流水线运行结束投递简讯
- **THEN** 投递以超时或连接错误失败，WARN 日志含渠道名与代理主机，运行历史状态不受影响

### Requirement: 流水线编辑页通知渠道多选

流水线编辑页 MUST 在名称、描述、启用字段下方渲染「通知」多选下拉控件，选项来自通知类凭据（`type_id` 为 `notify.feishu`/`notify.dingtalk`/`notify.generic`/`notify.email` 的凭据记录）。选项标签为「名称（id=N）」并附带 `provider` 信息辅助辨识。`notification_channels` 默认为空数组。编辑时 MUST 回显已选渠道。流水线复制操作 MUST 预填源流水线的 `notification_channels`。

#### Scenario: 通知多选下拉展示通知类凭据

- **WHEN** 用户在流水线编辑页展开「通知」多选下拉
- **THEN** 下拉选项仅列出通知类凭据（`notify.feishu`/`notify.dingtalk`/`notify.generic`/`notify.email`），不含 DNS/部署类凭据，每条选项可辨识凭据名称、ID 与渠道类型

#### Scenario: 选择多个渠道并保存

- **WHEN** 用户在多选中勾选两个渠道后保存流水线
- **THEN** 提交的 `notification_channels` 为选中的凭据 ID 数组，保存后响应中原样回显

#### Scenario: 编辑时回显已选渠道

- **WHEN** 用户打开一条 `notification_channels = [1, 3]` 的流水线编辑页
- **THEN** 「通知」多选下拉回显凭据 1 与凭据 3 的选中状态

#### Scenario: 复制流水线时预填通知渠道

- **WHEN** 用户复制一条 `notification_channels = [1, 3]` 的流水线
- **THEN** 新建表单中「通知」多选下拉预填凭据 1 与凭据 3 的选中状态
