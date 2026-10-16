# credential-forms Specification

## Purpose
界定凭据录入表单与连通性测试的行为：凭据类型如何对外部服务做轻量探测并脱敏呈现结果，凭据字段表单如何按 schema 扩展组织布局（多行文本域、整行字段、成对半宽字段）。

## Requirements

### Requirement: DNS 凭据类型支持连通性测试

四家内置 DNS 凭据类型（`cloud.cloudflare`、`cloud.aliyun`、`cloud.tencent`、`dns.tencent-eo`）MUST 实现连通性测试：用解密后的凭据对各家的**轻量身份接口**发起一次真实调用——Cloudflare 调用 `GET /user/tokens/verify`，阿里云与腾讯云体系调用 STS `GetCallerIdentity`——凭据有效且服务可达时返回可用，否则返回不可用并附原因。

#### Scenario: 腾讯云国际站凭据测试发往国际端点

- **WHEN** 对 `account_site` 为 `intl` 的腾讯云凭据执行测试
- **THEN** 探测请求发往 `sts.intl.tencentcloudapi.com`，国内站凭据发往 `sts.tencentcloudapi.com`

#### Scenario: 探测成功

- **WHEN** 凭据被服务端接受（签名与权限校验通过）
- **THEN** 返回「凭据可用」

#### Scenario: 探测失败的原因不回显密钥

- **WHEN** 探测因凭据不合法或服务不可达而失败
- **THEN** 返回不可用并附脱敏后的原因（含服务端错误码如 `AuthFailure.SignatureFailure`、Cloudflare `errors[].message`），原因中不得出现任何凭据字段值

### Requirement: 长文本凭据字段渲染为多行文本域

带 `x-multiline` 扩展的凭据字段 MUST 渲染成多行文本域（保留换行、可多行粘贴），而不是单行输入框。ACME 账号凭据的 `credentials` 字段（含账号私钥的 JSON）MUST 携带该扩展。

#### Scenario: credentials 字段多行编辑

- **WHEN** 在 ACME 账号凭据的创建/编辑表单中查看 `credentials` 字段
- **THEN** 该字段是多行文本域，粘贴的多行 JSON 保持换行原样提交

### Requirement: 凭据表单支持整行与成对半宽布局

凭据字段表单 MUST 按 schema 扩展组织宽度：带 `x-full-width` 的字段单独占整行；带 `x-end-row` 的半宽字段独占一行（半宽控件 + 空占位列换行）；未标注的标量字段两两一行（各占半宽）。ACME 账号凭据的 `ca` MUST 固定半宽独占一行（`x-end-row`），`directory_url` MUST 单独占整行（`x-full-width`），`eab_kid` 与 `eab_hmac_key` MUST 排成同一行；腾讯云 DNS 凭据的 `account_site` MUST 保持一列宽（半宽）打头并独占一行（行尾空占位列换行），`secret_id` 与 `secret_key` MUST 排成同一行。**凭据编辑弹窗中的"可用权限"能力多选字段为外层表单项（非 schema 驱动），MUST 独占一行展示，不与 `remark` 或 schema 字段同行。**

#### Scenario: directory_url 单独占整行

- **WHEN** 在 ACME 账号凭据表单中查看 `directory_url` 字段
- **THEN** 该字段独占一整行，不与其他字段并排

#### Scenario: EAB 密钥对排在同一行

- **WHEN** 在 ACME 账号凭据表单中查看 `eab_kid` 与 `eab_hmac_key` 字段
- **THEN** 两个字段各占半宽、排在同一行

#### Scenario: 腾讯云密钥对排在同一行

- **WHEN** 在腾讯云 DNS 凭据表单中查看 `secret_id` 与 `secret_key` 字段
- **THEN** 两个字段各占半宽、排在同一行

#### Scenario: 腾讯云站点选择打头且保持一列宽

- **WHEN** 在腾讯云 DNS 凭据表单中查看字段
- **THEN** `account_site` 按字段顺序（`x-field-order`）排在最前，控件保持一列宽；其行尾空占位列（`x-end-row`）把后续字段推到新行——`secret_id` 与 `secret_key` 成对两列排在第二行（与部署配置中 SSH 主机档案的视觉一致）

#### Scenario: 能力多选独占一行

- **WHEN** 在受控类型凭据表单中查看"可用权限"字段
- **THEN** 该字段独占一整行，不与备注或其他字段并排

### Requirement: ACME 账号 CA 选择包含 FreeSSL/Google 测试等预设及 EAB 必填与入口

ACME 账号凭据的 `ca` SHALL 提供 `freessl`（`https://acme.litessl.com/acme/v2/directory`）与 `google-test`（`https://dv.acme-v02.test-api.pki.goog/directory`）预设；当 `credentials` 为空或非合法 JSON 时，`freessl`、`zerossl`、`google`、`google-test` 四者的 `eab_kid`/`eab_hmac_key` 为必填，缺一即校验失败；当 `credentials` 为合法 JSON（已用 EAB 生成的有效账号凭据）时，该四者的 EAB 字段不再必填，签发复用 `credentials` 的 KID/私钥与 Directory URL，不再使用 EAB 重新生成。该四值对 `ca` 行尾 SHALL 按值展示外部链接：`freessl → https://freessl.cn/automation/eab-manager`、`zerossl → https://app.zerossl.com/developer`、`google/google-test → https://docs.cloud.google.com/certificate-manager/docs/public-ca-tutorial#request-key-hmac`，其他 CA 不展示。

#### Scenario: 选择 freessl 预设填充 Directory URL

- **WHEN** `ca == freessl`
- **THEN** `directory_url` 默认值为 `https://acme.litessl.com/acme/v2/directory`

#### Scenario: 选择 google-test 预设填充测试 Directory

- **WHEN** `ca == google-test`
- **THEN** `directory_url` 默认值为 `https://dv.acme-v02.test-api.pki.goog/directory`

#### Scenario: 未建立账号时需 EAB 的 CA 缺字段校验失败

- **WHEN** `ca` 为 `freessl`/`zerossl`/`google`/`google-test`，且 `credentials` 为空或非合法 JSON，`eab_kid` 或 `eab_hmac_key` 为空
- **THEN** 表单校验失败，提示对应字段必填

#### Scenario: 已建立账号时 EAB 可空

- **WHEN** `ca` 为上述四值之一，且 `credentials` 为合法 JSON（含有效 KID/私钥）
- **THEN** `eab_kid`/`eab_hmac_key` 允许为空，提交通过，后续签发复用 `credentials` 不再使用 EAB

#### Scenario: 按 CA 展示对应 EAB 入口

- **WHEN** `ca == zerossl` / `google` / `google-test` / `freessl`
- **THEN** `ca` 行尾分别展示对应 URL 的链接

#### Scenario: 非 EAB CA 不展示入口

- **WHEN** `ca` 为 `letsencrypt`/`letsencrypt-staging`/`custom` 等非上述四值
- **THEN** 不展示 EAB 入口

### Requirement: ACME 账号 CA 选择固定半宽独占一行

ACME 账号凭据（`acme.account`）的 `ca` 字段 MUST 固定占 1/2 列——半宽控件（`span 12`）+ 行尾空占位列（`x-end-row: true`）独占一行，不与同表单的其他字段挤在同一行；长渠道名称（如 `Google Trust Services (测试)`）不得被截断或省略。

#### Scenario: CA 下拉固定半宽且独占一行

- **WHEN** 在凭据页新建或编辑 ACME 账号凭据并展开字段表单
- **THEN** `ca` 字段的控件列为半宽（`span 12`），其后有一空占位列（`aria-hidden`、`span 12`），下一个字段（`directory_url`）从新行开始

#### Scenario: 长渠道名称完整可见

- **WHEN** `ca` 选中名称较长的渠道（如 `Google Trust Services (测试)`、`Let's Encrypt (测试)`）
- **THEN** 下拉已选项与下拉面板中的标签完整展示，不出现省略号截断或被相邻字段挤压隐藏

#### Scenario: 字段顺序保持 ca 打头

- **WHEN** 渲染 ACME 账号凭据表单
- **THEN** 字段按 `x-field-order: ["ca", "directory_url", "eab_kid", "eab_hmac_key", "credentials"]` 顺序呈现，`ca` 始终在首行首列

#### Scenario: 选择 acme.account 类型后 CA 字段占比一半

- **WHEN** 在凭据新建/编辑弹窗中将类型切换为 `acme.account`
- **THEN** `CA` 下拉框的表单列宽度为一半（`span 12`）且独占一行，右侧为空占位列；与现有“固定半宽独占一行”视觉一致，不因切换类型而变为整行或与其他字段同行

### Requirement: 受控凭据类型展示能力多选

凭据新建/编辑弹窗在 `type_id` 为 `cloud.tencent` / `cloud.cloudflare` / `cloud.aliyun` 时 SHALL 在类型选择之后、备注之前展示"可用权限"多选框（选项 `DNS` / `部署`，对应值 `dns` / `deploy`），至少选中一项方可提交；新建时默认两项均选中，编辑时回填已存值。非受控类型不展示该字段。

#### Scenario: 受控类型出现多选且默认全选

- **WHEN** 新建凭据并选择 `cloud.tencent` / `cloud.cloudflare` / `cloud.aliyun`
- **THEN** 表单出现"可用权限"多选框，`DNS` 与 `部署` 均已勾选

#### Scenario: 非受控类型不出现多选

- **WHEN** 选择 `dns.tencent-eo` / `deploy.ssh` / `acme.account` 等类型
- **THEN** 表单不展示"可用权限"字段

### Requirement: S3 兼容存储凭据字段按业务分组布局

`S3 兼容存储`（`type_id: deploy.s3`）凭据的字段表单 MUST 按业务语义分组为三行、每行两列呈现，顺序固定为 `access_key`、`secret_key`、`endpoint`、`bucket`、`region`、`prefix`，`session_token` 置于末尾。分组为：第一行 `access_key | secret_key`（密钥对）、第二行 `endpoint | bucket`（接入与桶）、第三行 `region | prefix`（区域与前缀）；三行均为未标注的标量半宽字段、两两一行的默认流式布局（`span 12`），不得对其中任一字段标注 `x-full-width` 或 `x-end-row`；`session_token` 保持默认半宽、排在所有分组之后（若为空仍占位但不影响前三行的两列成行）。

#### Scenario: S3 字段按分组顺序渲染

- **WHEN** 在凭据新建/编辑弹窗中选择类型 `S3 兼容存储` 并展开字段表单
- **THEN** 字段按 `x-field-order: ["access_key","secret_key","endpoint","bucket","region","prefix","session_token"]` 顺序呈现

#### Scenario: 三行两列成对布局

- **WHEN** 查看 S3 凭据表单的字段排列
- **THEN** `access_key` 与 `secret_key` 在同一行各占半宽；`endpoint` 与 `bucket` 在同一行各占半宽；`region` 与 `prefix` 在同一行各占半宽；三行之间自动换行，不出现某字段独占整行或半宽独占一行的情况

#### Scenario: session_token 置于末尾且不打乱分组

- **WHEN** 查看 S3 凭据表单的末尾字段
- **THEN** `session_token` 排在 `prefix` 之后、为半宽字段；其存在与否不影响前三行每行两列的成对布局

### Requirement: 凭据类型下拉展示名不带括号及使用范围标注

凭据新建/编辑弹窗的类型下拉（`Select` 的 `options[].label`）MUST 直接展示各凭据类型的短名称，不再用中文括号 `（…）` 追加使用范围标注。受影响类型的展示名 MUST 为：`cloud.tencent → 腾讯云`、`dns.tencent-eo → 腾讯云 EdgeOne`、`cloud.cloudflare → Cloudflare`、`cloud.aliyun → 阿里云`、`deploy.ssh → SSH 主机`；`deploy.s3 → S3 兼容存储`、`deploy.webdav → WebDAV`、`acme.account → ACME 账号` 等原本无括号的类型保持不变。`GET /api/credential-types` 返回的 `display_name` MUST 与下拉展示一致（同一来源）。

#### Scenario: 下拉不再出现括号标注

- **WHEN** 打开凭据新建/编辑弹窗并展开类型下拉
- **THEN** 选项文本中不出现 `（` / `）` 及其包裹的使用范围字样（如 `DNS`、`部署`、`DNS、部署`）；以 `腾讯云`、`Cloudflare` 等短名称呈现

#### Scenario: 五类凭据的新展示名

- **WHEN** 查看类型下拉的选项列表
- **THEN** `cloud.tencent` 显示为 `腾讯云`、`dns.tencent-eo` 显示为 `腾讯云 EdgeOne`、`cloud.cloudflare` 显示为 `Cloudflare`、`cloud.aliyun` 显示为 `阿里云`、`deploy.ssh` 显示为 `SSH 主机`

#### Scenario: 后端接口与前端下拉一致

- **WHEN** 调用 `GET /api/credential-types`
- **THEN** 返回的 `display_name` 即为下拉所用文本，对上述五类凭据不再含括号标注

### Requirement: 枚举字段控件满列宽且帮助链接附于标签提示图标右侧

SchemaForm 渲染枚举（string enum）字段为 `Select` 时，控件 MUST 撑满其所在列（与 `Input` 控件的宽度行为一致），不得因任何伴随元素分宽而变窄。字段携带 `x-help-links` 且按当前取值命中时，文档链接 MUST 渲染在字段标签的「?」提示图标右侧；此时渲染器 MUST 自绘提示图标并接管该字段的 tooltip（提示内容复用字段 `description`）。链接不得与控件同行分宽（避免挤压半宽字段控件），也不得置于控件下方。

#### Scenario: ca 下拉与 EAB 输入框同宽

- **WHEN** 编辑 `acme.account` 凭据并查看 `ca` 字段的下拉控件
- **THEN** `ca` 下拉控件占满其 `span 12` 列，宽度与同行其它半宽输入控件（如 `eab_kid` 输入框）一致，未被「申请 HMAC」等链接挤压

#### Scenario: 帮助链接在提示图标右侧

- **WHEN** `ca` 的当前值带有帮助链接（如 `google → 申请 HMAC`）
- **THEN** 链接出现在标签「ca ?」的问号图标右侧，悬停问号图标仍显示字段说明；`ca` 取值为 `letsencrypt` 等无链接项时，标签恢复默认形态（文本 + antd 提示图标）

#### Scenario: 链接不与控件同行分宽、不置于控件下方

- **WHEN** 查看带帮助链接的枚举字段表单
- **THEN** 控件所在行不被链接分宽，链接也不出现在控件的下方

### Requirement: WebDAV 凭据字段两行两列布局

`WebDAV`（`type_id: deploy.webdav`）凭据的字段表单 MUST 按两行两列呈现：第一行 `url | base_path`、第二行 `username | password`，每行两字段各占半宽（`span 12`）默认流式布局。顺序 MUST 由后端 `x-field-order: ["url","base_path","username","password"]` 钉住，四个字段均不标注 `x-full-width` / `x-end-row` / `x-multiline`，与 S3 三行两列同为「未标注即两两同行」的成对布局。

#### Scenario: WebDAV 字段按两行两列顺序渲染

- **WHEN** 在凭据新建/编辑弹窗中选择类型 `WebDAV` 并展开字段表单
- **THEN** 字段按 `x-field-order` 顺序呈现为 `url`、`base_path`、`username`、`password`，`url` 与 `base_path` 在同一行各占半宽，`username` 与 `password` 在同一行各占半宽

#### Scenario: 不对 WebDAV 字段标注独占行或整行宽度

- **WHEN** 查看 `deploy.webdav` 的 `fields_schema`
- **THEN** 四个字段均未携带 `x-full-width`、`x-end-row` 或 `x-multiline`，两行之间自动换行，不出现某字段整行独占或半宽独占一行的情况

#### Scenario: 字段顺序由后端 schema 保证

- **WHEN** 解析 `deploy.webdav` 的 `fields_schema` 顶层扩展
- **THEN** `x-field-order` 为 `["url","base_path","username","password"]`，不受 `properties` 字母序重排影响

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

### Requirement: 通知凭据连通性测试入口

通知凭据的连通性测试 MUST 通过凭据列表页或详情页的测试入口触发（`POST /api/credentials/{id}/test`），与 DNS/部署凭据测试入口一致。测试结果 MUST 经脱敏后呈现。

#### Scenario: 测试入口与现有凭据一致

- **WHEN** 用户在凭据列表页对一条 `notify.feishu` 凭据点击测试按钮
- **THEN** 触发 `POST /api/credentials/{id}/test`，飞书机器人收到测试消息，结果以脱敏文本呈现
