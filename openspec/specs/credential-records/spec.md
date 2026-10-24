# credential-records Specification

## Purpose
为凭据记录提供稳定的标识与人类可读的备注，使同一平台多对密钥在列表与流水线凭据下拉中可被区分、追溯，避免凭据选择错误。

## Requirements

### Requirement: 凭据记录的唯一标识暴露

每条凭据记录 SHALL 拥有一个稳定的唯一标识（ID），并在凭据列表 API 的每条记录中返回，使调用方与前端可据此稳定引用。该 ID MUST 在凭据记录的生命周期内不变，且不得复用于其他记录。凭据列表与详情响应 SHALL 额外携带 `capabilities` 字段（受控类型为 `dns`/`deploy` 子集，非受控类型为空或缺省），与 ID 一同作为流水线侧过滤与校验的依据。凭据类型的展示名（`display_name`）由 `GET /api/credential-types` 返回，前端下拉与列表展示该值；展示名不再用括号追加使用范围，`cloud.tencent`/`dns.tencent-eo`/`cloud.cloudflare`/`cloud.aliyun`/`deploy.ssh` 的展示名分别为 `腾讯云`/`腾讯云 EdgeOne`/`Cloudflare`/`阿里云`/`SSH 主机`。

#### Scenario: 列表每条记录含 ID

- **WHEN** 调用凭据列表 API
- **THEN** 返回的每条凭据记录都包含一个 ID 字段

#### Scenario: ID 在更新后保持不变

- **WHEN** 一张凭据被更新（改名或改字段）
- **THEN** 该凭据的 ID 不变

#### Scenario: 列表每条受控记录含能力

- **WHEN** 调用凭据列表 API 且存在 `cloud.tencent` 凭据
- **THEN** 返回的对应记录包含 `capabilities` 数组

#### Scenario: 类型展示名不带括号

- **WHEN** 调用 `GET /api/credential-types` 并查看 `cloud.tencent`/`cloud.cloudflare` 等类型的 `display_name`
- **THEN** 返回值不含 `（`/`）` 括号及其中使用范围字样，分别为 `腾讯云`/`腾讯云 EdgeOne`/`Cloudflare`/`阿里云`/`SSH 主机`

### Requirement: 凭据备注字段

凭据记录 SHALL 携带一个可选的备注字段（`remark`），用于人类可读的用途、归属或环境说明（如「生产 DNS」「测试 CLB」）。该字段可为空，默认为空，不参与凭据解析或厂商认证。创建、读取、更新凭据的 API MUST 透传该字段；删除凭据时该字段随之移除。**创建与更新请求 MAY 携带 `capabilities` 字段（仅受控类型生效），服务端 MUST 按能力集合校验并持久化。**

#### Scenario: 创建凭据时携带备注

- **WHEN** 创建凭据请求体包含 `remark`
- **THEN** 该凭据记录被持久化，列表与详情 API 返回该备注

#### Scenario: 备注缺省为空

- **WHEN** 创建凭据请求未提供 `remark`
- **THEN** 该凭据记录的备注为空字符串，列表与详情 API 返回空

#### Scenario: 更新备注

- **WHEN** 对一张已存在凭据发起更新，请求体携带新的 `remark`
- **THEN** 该凭据的备注被替换为新值

#### Scenario: 创建凭据时携带能力

- **WHEN** 创建 `tencent` 凭据且请求体包含 `capabilities: ["deploy"]`
- **THEN** 该凭据的 `capabilities` 被持久化，列表与详情返回该值

### Requirement: 凭据列表默认按 ID 降序

凭据列表 API 返回的记录 MUST 按 ID 降序排列，ID 最大的凭据排在最前。首次加载凭据列表页时 MUST 以 ID 降序展示结果。

#### Scenario: 首次进入凭据列表按 ID 降序

- **WHEN** 用户首次进入凭据列表页
- **THEN** 返回的列表中 ID 最大的凭据显示在最前

#### Scenario: 翻页保持 ID 降序稳定

- **WHEN** 凭据列表有多页数据且用户翻到第 2 页
- **THEN** 第 2 页的记录 ID 均小于第 1 页的最大 ID，各页之间不重复、不遗漏

### Requirement: 流水线凭据下拉以 ID 与备注辅助区分

在流水线步骤编辑表单中，凭据选择下拉的每个选项 SHALL 在凭据名称之外展示其 ID 与备注（若有），形式为「名称（id=N）」并在存在备注时附加备注文本，使同一类型多张凭据可被区分。备注为空时仅展示名称与 ID。

#### Scenario: 同类型多张凭据可区分

- **WHEN** 同一凭据类型下存在多张凭据，且至少一张填写了备注
- **THEN** 凭据下拉的每个选项展示名称、ID 与备注，用户可据此区分不同凭据

#### Scenario: 无备注时仅展示名称与 ID

- **WHEN** 一张凭据未填写备注
- **THEN** 凭据下拉的对应选项仅展示「名称（id=N）」，不出现空备注标记

### Requirement: 凭据编辑弹窗的数据新鲜度与隔离

打开凭据编辑弹窗时，弹窗内展示的表单数据 SHALL 始终对应当前被点击编辑的那条凭据记录，不得展示此前编辑过的其他记录的陈旧数据；不同记录间切换编辑时，弹窗 MUST 清空或遮蔽旧数据直至新记录的详情加载完成，且切换过程中不得将一条记录的字段值写入另一条记录。

#### Scenario: 连续编辑两条不同凭据互不串扰

- **WHEN** 用户在凭据列表中点击某条记录（如 #1 letsencrypt-staging）的“编辑”，随后（无论是否先关闭弹窗）再点击另一条记录（如 #16 ZeroSSL）的“编辑”
- **THEN** 第二次打开的编辑弹窗中，名称、类型、备注与类型驱动的字段区均展示第二条记录的现值（以 `GET /api/credentials/{id}` 返回为准），不包含第一条记录的任何字段残留

#### Scenario: 详情加载期间不展示陈旧数据

- **WHEN** 编辑弹窗已打开但目标记录的详情接口仍在加载中
- **THEN** 表单不展示上一条记录的旧值（表现为清空、骨架或加载态），且“保存”操作在详情加载完成前不可提交或提交时以新记录现值为基准

#### Scenario: 快速切换编辑目标以最后一次选择为准

- **WHEN** 用户快速连续点击多条记录的“编辑”，多个详情请求并发或乱序返回
- **THEN** 最终展示的数据对应最后一次被点击的记录 ID，过期请求的响应不得覆盖当前弹窗内容

### Requirement: 凭据列表展示能力标签

凭据列表 SHALL 新增"可用权限"列：对 `cloud.tencent` / `cloud.cloudflare` / `cloud.aliyun` 凭据展示其 `capabilities` 对应的标签（`DNS` / `部署`），双选时两标签并列；非受控类型或能力为空时显示 `—`。

#### Scenario: 受控凭据展示能力标签

- **WHEN** 列表中存在 `capabilities: ["dns","deploy"]` 的 `cloud.tencent` 凭据
- **THEN** 该行"可用权限"列展示 `DNS` 与 `部署` 两个标签

#### Scenario: 单能力仅展示对应标签

- **WHEN** 某 `cloud.aliyun` 凭据为 `["dns"]`
- **THEN** 该行仅展示 `DNS` 标签

#### Scenario: 非受控类型显示占位

- **WHEN** 列表中存在 `deploy.ssh` / `dns.tencent-eo` 凭据
- **THEN** 其"可用权限"列显示 `—`

### Requirement: 流水线凭据下拉可读性保留能力上下文

流水线步骤表单中按能力过滤后的凭据下拉 SHALL 仍保留现有可读性约定（名称 + `id` + 备注），不因能力过滤而丢失备注展示；能力本身不在下拉标签中重复展示（过滤已体现）。

#### Scenario: 过滤后下拉仍带备注

- **WHEN** `dns_credential_id` 下拉经能力过滤后展示某张带备注的 `cloud.cloudflare` 凭据
- **THEN** 选项仍展示名称、`id` 与备注

### Requirement: 凭据列表“可用权限”列保持左对齐并以字符串形式展示

凭据列表的"可用权限"列 MUST 保持与表格其它列一致的默认左对齐，不得将单元格内容设置为居中对齐（`align: center`）。受控类型的能力值 MUST 以顿号「、」连接的单个字符串形式展示（如同时具备 `dns` 与 `deploy` 时显示 `DNS、部署`），MUST NOT 使用多个标签（Tag）并排渲染；非受控类型或能力为空时仍显示占位符 `—`，同样左对齐。

#### Scenario: 可用权限列不居中

- **WHEN** 查看凭据列表的"可用权限"列
- **THEN** 该列单元格内容为左对齐，与"名称""类型""备注"等列的对齐方式一致，不出现居中对齐

#### Scenario: 多能力以字符串展示

- **WHEN** 某 `cloud.tencent` 凭据同时具备 `dns` 与 `deploy` 能力
- **THEN** 该列显示字符串 `DNS、部署` 并居左，不出现并排的标签组件

### Requirement: 存量凭据 type_id 自动迁移

系统升级后 SHALL 通过数据库迁移将 `acmecast_credential.type_id` 的旧值重写为新值：`cloudflare→cloud.cloudflare`、`aliyun→cloud.aliyun`、`tencent→cloud.tencent`、`tencent-eo→dns.tencent-eo`、`ssh→deploy.ssh`、`s3→deploy.s3`、`webdav→deploy.webdav`（`acme.account` 不动）。迁移 MUST 幂等（重复执行不再改已是新值或不命中旧值的行），且对空库无影响。

#### Scenario: 存量凭据在升级后可被新注册表识别

- **WHEN** 库中存在旧 `type_id` 的凭据记录，应用完成迁移后查询凭据
- **THEN** 其 `type_id` 已为新值，且可在凭据类型注册表中被识别

#### Scenario: 迁移幂等

- **WHEN** 迁移已执行过，再次执行
- **THEN** 库中已是新值的行保持不变，不产生额外变更

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

### Requirement: 凭据列表返回代理模式

凭据列表 API（`GET /api/credentials`）的每条记录 SHALL 携带 `proxy_mode` 字段，取值为 `direct` / `inherit` / `custom`，使前端无需打开编辑弹窗即可了解该凭据的出网代理方式。该字段 MUST 以明文列存储（与 `capabilities` 同口径：从加密字段中提取的非敏感枚举值单独持久化），列表 API 不得为返回该字段而解密凭据。创建与更新凭据时（`POST` / `PUT /api/credentials/{id}`），服务端 MUST 从请求 `fields.proxy_mode` 归一化后写入明文列；`fields.proxy_mode` 缺失但 `fields.proxy_url` 非空时视为 `custom`，两者均缺时视为 `direct`（与运行时 `ProxyMode::from_credentials` 同口径）。凭据详情响应（`GET /api/credentials/{id}`）同样返回该字段。`proxy_mode` 明文列不得携带 `proxy_url` 值——代理 URL 仍在 `encrypted_fields` 加密存储，列表响应不暴露。

#### Scenario: 列表每条记录含代理模式

- **WHEN** 调用凭据列表 API 且存在 `proxy_mode=custom` 的凭据
- **THEN** 返回的对应记录包含 `proxy_mode: "custom"`

#### Scenario: 直连凭据返回 direct

- **WHEN** 凭据 `proxy_mode=direct`（缺省）
- **THEN** 列表与详情响应中 `proxy_mode` 为 `"direct"`

#### Scenario: 创建凭据时携带代理模式

- **WHEN** 创建凭据请求 `fields.proxy_mode = "inherit"`
- **THEN** 该凭据记录的明文 `proxy_mode` 列为 `inherit`，列表与详情返回该值

#### Scenario: 更新代理模式后列表反映新值

- **WHEN** 将某凭据从 `direct` 更新为 `custom`（含 `proxy_url`）
- **THEN** 更新后的列表响应中该记录 `proxy_mode` 为 `"custom"`

#### Scenario: 存量凭据缺字段视为直连

- **WHEN** 存量凭据加密字段中不含 `proxy_mode` 与 `proxy_url`，迁移后
- **THEN** 其明文 `proxy_mode` 列为 `"direct"`

#### Scenario: 存量凭据有旧 proxy_url 视为 custom

- **WHEN** 存量凭据加密字段含 `proxy_url` 但无 `proxy_mode`，迁移后
- **THEN** 其明文 `proxy_mode` 列为 `"custom"`

#### Scenario: 列表不暴露代理 URL

- **WHEN** 凭据 `proxy_mode=custom` 且 `proxy_url=socks5://user:pass@host:port`
- **THEN** 列表响应仅返回 `proxy_mode: "custom"`，不包含 `proxy_url` 值

### Requirement: 凭据列表展示代理列

凭据列表页 SHALL 新增「代理」列，按凭据 `proxy_mode` 展示中文标签：`direct`→「直连」、`inherit`→「继承全局」、`custom`→「自定义」。该列 MUST 保持与「可用权限」「名称」「备注」等列一致的默认左对齐，不得居中对齐。展示形式为纯文本（`Typography.Text`），不得使用标签（Tag）并排渲染。

#### Scenario: 自定义代理显示对应标签

- **WHEN** 列表中存在 `proxy_mode=custom` 的凭据
- **THEN** 该行「代理」列显示「自定义」

#### Scenario: 继承全局显示对应标签

- **WHEN** 列表中存在 `proxy_mode=inherit` 的凭据
- **THEN** 该行「代理」列显示「继承全局」

#### Scenario: 直连显示对应标签

- **WHEN** 列表中存在 `proxy_mode=direct` 的凭据
- **THEN** 该行「代理」列显示「直连」

#### Scenario: 代理列左对齐

- **WHEN** 查看凭据列表的「代理」列
- **THEN** 该列单元格内容为左对齐，与「名称」「类型」等列对齐方式一致

### Requirement: 存量凭据 proxy_mode 回填迁移

系统升级时 SHALL 通过数据库迁移为 `acmecast_credential` 表新增 `proxy_mode` 明文列，并回填存量凭据：从 `encrypted_fields` 解密后按 `ProxyMode::from_credentials` 归一化（旧 `proxy_url` 有值→`custom`，无值或缺字段→`direct`），写入明文列。迁移 MUST 幂等（重复执行不改已回填的值），且对空库无影响。迁移需要凭据加密密钥（`CredentialCipher`）来解密存量 `encrypted_fields`——迁移 MUST 在密钥已初始化的环境下执行，密钥未配置时迁移跳过回填（列仍以默认 `direct` 建立），待密钥配置后下次启动补回填。

#### Scenario: 存量凭据迁移后可从列表读代理模式

- **WHEN** 库中存在变更前保存的凭据记录，迁移完成且密钥已配置
- **THEN** 其明文 `proxy_mode` 列按旧 `proxy_url` 有无归一化为 `custom` 或 `direct`，列表 API 返回该值

#### Scenario: 迁移幂等

- **WHEN** 迁移已执行过，再次执行
- **THEN** 库中已有值的行保持不变

#### Scenario: 空库迁移无影响

- **WHEN** 迁移在空库上执行
- **THEN** 仅创建 `proxy_mode` 列，无行需回填，不报错
