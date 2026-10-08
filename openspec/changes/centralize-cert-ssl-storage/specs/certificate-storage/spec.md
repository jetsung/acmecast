# Spec Delta — certificate-storage

## MODIFIED Requirements

### Requirement: 有效证书扁平集中存储于活跃目录

有效证书（未吊销、未被续期替换）的证书链与私钥文件 MUST 统一扁平落在数据目录的活跃目录 `certs/ssl/` 下，文件名规则保持不变（`<主域名>.cert.pem` / `<主域名>.key.pem`，通配符 `*` 替换为 `_`，多域名取申请时的第一个域名为主域名）。数据库记录的 `cert_pem_path` / `key_pem_path` MUST 指向该活跃目录下的相对路径（不含指纹段）。`certs/ssl/` 下同主域名的证书 MUST 仅保留最新一份。

#### Scenario: 新申请证书落盘到活跃目录

- **WHEN** 系统为指纹 `fp-1`、主域名 `sub.example.com` 的证书执行落盘，且 `certs/ssl/` 下不存在同名文件
- **THEN** 证书链写入 `certs/ssl/sub.example.com.cert.pem`，私钥写入 `certs/ssl/sub.example.com.key.pem`，数据库记录的两个路径字段与之完全一致

#### Scenario: 扁平覆盖仅保留最新

- **WHEN** `certs/ssl/sub.example.com.cert.pem` 已存在（指纹 `fp-old`），再次为同一主域名申请到新证书（指纹 `fp-new`）并落盘
- **THEN** 旧证书的证书链与私钥先被移动到 `certs/archived/fp-old/sub.example.com.cert.pem` 与 `certs/archived/fp-old/sub.example.com.key.pem`，新证书写入 `certs/ssl/sub.example.com.cert.pem` 与 `certs/ssl/sub.example.com.key.pem`，数据库记录指向新的活跃路径

#### Scenario: 不同主域名并存于扁平目录

- **WHEN** 系统持有主域名分别为 `a.example.com` 与 `b.example.com` 的有效证书
- **THEN** 两者分别位于 `certs/ssl/a.example.com.cert.pem` 与 `certs/ssl/b.example.com.cert.pem`，互不覆盖

### Requirement: 续期或重签时旧证书归档到 archived

当同一主域名的证书被续期或重签替换时（即 `certs/ssl/` 下已存在同名证书文件），系统 MUST 在写入新证书前将旧证书的证书链与私钥按**旧证书指纹**移动到归档目录 `certs/archived/<旧指纹>/` 下，文件名保持不变。归档 MUST 参考 `revoked` 的实现方式：按指纹分子目录、权限受限、路径防逃逸。

#### Scenario: 续期归档旧证书

- **WHEN** 对主域名 `sub.example.com` 的证书续期成功，旧证书指纹为 `fp-old`，新证书指纹为 `fp-new`
- **THEN** 旧证书文件出现在 `certs/archived/fp-old/sub.example.com.cert.pem` 与 `certs/archived/fp-old/sub.example.com.key.pem`，新证书文件位于 `certs/ssl/sub.example.com.cert.pem` 与 `certs/ssl/sub.example.com.key.pem`

#### Scenario: 归档目标已存在时覆盖

- **WHEN** `certs/archived/fp-old/` 已存在旧归档材料
- **THEN** 本次归档覆盖该指纹目录下的同名文件，不报错

#### Scenario: 归档失败不阻断新证书写入

- **WHEN** 旧证书归档发生 IO 错误（如源文件缺失）
- **THEN** 系统记录错误日志，仍继续写入新证书到 `certs/ssl/`，不使本次申请失败；归档失败的旧文件留在原位或按日志可追溯

### Requirement: 吊销时归档证书文件

证书吊销成功后，系统 MUST 把该证书的证书链与私钥文件从 `certs/ssl/` 扁平路径移动到吊销目录 `certs/revoked/<指纹>/` 下（按被吊销证书的指纹分子目录），并把数据库记录的相对路径同步更新为新位置。系统 MUST NOT 在吊销时删除证书文件。`certs/ssl`、`certs/archived/<指纹>/`、`certs/revoked/<指纹>/` 在 `certs/` 下同级。

#### Scenario: 吊销证书后文件归档

- **WHEN** 一张位于 `certs/ssl/sub.example.com.cert.pem`（指纹 `fp-1`）的有效证书被成功吊销
- **THEN** 其证书链与私钥文件出现在 `certs/revoked/fp-1/sub.example.com.cert.pem` 与 `certs/revoked/fp-1/sub.example.com.key.pem`，`certs/ssl/` 下的同名文件不再存在，数据库记录指向吊销目录下的新位置
### Requirement: 吊销后文件仍可读取与下载

吊销后证书详情与文件下载 MUST 继续可用——文件路径归档与数据库路径更新必须同时生效，不允许出现数据库指向的文件不存在。

#### Scenario: 吊销后下载证书

- **WHEN** 用户对一张已归档的吊销证书调用下载接口
- **THEN** 接口返回其证书链内容（读取自 `certs/revoked/<指纹>/`）

### Requirement: 归档失败时保持一致性

吊销动作对 CA 而言不可逆；吊销文件归档失败（如文件缺失、IO 错误）MUST NOT 使吊销结果报错回滚：系统 SHALL 记录错误日志、保持数据库路径不变（文件留在 `certs/ssl/` 原位，下载仍可用），并照常返回吊销成功。

#### Scenario: 文件移动失败时的降级

- **WHEN** 吊销成功但文件移动发生 IO 错误
- **THEN** 接口仍返回吊销成功，数据库路径保持 `certs/ssl/...` 不变，文件仍留在活跃目录，日志中记录归档失败

### Requirement: 归档保持受限权限

活跃目录 `certs/ssl/`、归档目录 `certs/archived/<指纹>/`、吊销目录 `certs/revoked/<指纹>/` 及证书文件 MUST 维持与数据目录一致的受限权限：目录 `0700`、文件 `0600`。

#### Scenario: 归档后的权限

- **WHEN** 证书文件被移动到 `certs/revoked/<指纹>/` 或 `certs/archived/<指纹>/`
- **THEN** 该目录对属主之外无任何权限，文件保持属主可读写

#### Scenario: 活跃目录的权限

- **WHEN** 证书文件写入 `certs/ssl/`
- **THEN** 该目录以 `0700` 创建或收紧，文件以 `0600` 落盘，创建时即受限而非事后 chmod

### Requirement: 存量证书迁移到扁平活跃目录

系统 MUST 提供从旧布局 `certs/<指纹>/`（指纹直接位于 `certs/` 下、非 `revoked`/`ssl`/`archived`）到新布局 `certs/ssl/` 的一次性迁移能力：迁移 MUST 将存量证书文件扁平化移动到 `certs/ssl/`，同主域名出现多指纹冲突时仅保留最新（按 `not_after` 或 `created_at` 最大判定），其余按指纹移入 `certs/archived/<指纹>/`，并同步更新数据库路径记录；迁移 MUST 保持幂等（已在新位置的记录不再移动，重复执行无副作用）。

#### Scenario: 启动时迁移旧布局证书

- **WHEN** 数据目录中存在 `certs/fp-old/sub.example.com.cert.pem` 且数据库记录指向 `certs/fp-old/...`
- **THEN** 迁移后文件位于 `certs/ssl/sub.example.com.cert.pem`，数据库记录更新为 `certs/ssl/sub.example.com.cert.pem`，原 `certs/fp-old/` 目录不再保留

#### Scenario: 同主域名多指纹冲突仅保留最新

- **WHEN** 旧布局中 `certs/fp-old/sub.example.com.cert.pem` 与 `certs/fp-new/sub.example.com.cert.pem` 主域名相同
- **THEN** 迁移后 `certs/ssl/sub.example.com.cert.pem` 为最新证书（`not_after` 最大者），较旧者移入 `certs/archived/<旧指纹>/sub.example.com.cert.pem`，数据库记录分别指向对应位置

#### Scenario: 迁移幂等

- **WHEN** 某证书已位于 `certs/ssl/sub.example.com.cert.pem` 且数据库已指向新路径
- **THEN** 再次执行迁移不移动文件、不改写数据库

#### Scenario: 吊销证书不参与活跃迁移

- **WHEN** 某证书已归档于 `certs/revoked/fp-1/`
- **THEN** 迁移不触碰该证书的文件与数据库记录

## REMOVED Requirements

无删除，仅对既有 Requirement 的路径语义进行变更（`certs/<指纹>/` → `certs/ssl/` 扁平，新增 `certs/archived/<指纹>/` 归档语义）。
