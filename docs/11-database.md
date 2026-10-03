# 数据库与数据目录

## 数据库

支持三种方言，由连接串 scheme 判定（`ACMECAST_DATABASE_URL` 或
`server.database_url`）：

| 方言 | 连接串示例 |
|---|---|
| SQLite（默认） | `sqlite://./data/acmecast.db?mode=rwc`（也支持 `sqlite::memory:`） |
| MySQL / MariaDB | `mysql://user:pass@host:3306/acmecast` |
| PostgreSQL | `postgres://user:pass@host:5432/acmecast` |

其它 scheme 启动即报「不支持的数据库方言」。

- SQLite：建连前自动创建父目录。
- MySQL：连接池设 1 小时空闲回收（抵消 MySQL 8 小时等待超时的静默断连）。
- 连接池：`max_connections` 默认 10，`min_connections` 1，连接超时 10 秒。

### 迁移

启动时自动执行版本化迁移（当前 5 个版本）。迁移定义只有一份，由 SeaQuery
按方言自动翻译（自增主键、布尔、时间、JSON 类型各有方言映射；PostgreSQL
另有一个迁移把时间列统一转成 `timestamptz`）。迁移失败会**报出失败的版本号**
并中止启动。

设计约束：已应用的迁移永不修改，schema 变更一律新增迁移版本。三方言行为
一致性由 CI 的 `dialect_equivalence` 测试保证。

### 表结构概览

表名统一 `acmecast_` 前缀，时间列一律 UTC：

| 表 | 内容 |
|---|---|
| `acmecast_pipeline` | 流水线（名称、启用、描述） |
| `acmecast_pipeline_step` | 步骤（`order_index`、`type_id`、`input` JSON、启用） |
| `acmecast_history` | 运行历史（触发来源、状态、起止时间、错误摘要） |
| `acmecast_history_log` | 步骤日志（步骤序号、级别、消息、时间） |
| `acmecast_storage` | 流水线级 KV 状态（`(pipeline_id, store_key)` 唯一） |
| `acmecast_credential` | 凭据（`type_id` + `encrypted_fields` 密文） |
| `acmecast_cert` | 证书记录（域名集合、指纹、起止时间、文件路径、吊销时间、签发账号） |
| `acmecast_schedule` | 调度（cron、续期域名、上次/下次触发时间） |
| `acmecast_trigger_log` | 触发审计（来源 cron/renewal、详情、时间） |
| `acmecast_deployment` | 部署记录（目标键、指纹、路径、重载输出） |

外键默认级联删除；凭据的 `encrypted_fields` 是 AES-256-GCM 密文的 base64。

## 数据目录

数据目录（默认 `./data`，容器内 `/data`）即**全部持久化状态**：

```
data/
├── acmecast.db                 # SQLite 默认库（用 MySQL/PG 时无此文件）
├── config.toml                 # 自动生成的配置模板
└── certs/
    ├── <指纹>/
    │   ├── <主域名>.cert.pem   # 证书链
    │   └── <主域名>.key.pem    # 私钥（0600）
    └── revoked/<指纹>/         # 已吊销证书的归档（只换目录不改名）
```

文件名规则：主域名作前缀，通配符 `*` 换成 `_`
（`*.example.com` → `_.example.com.cert.pem`）；多域名取第一个。

### 权限

- 数据目录打开时若权限过宽（对属主之外有任何放行）会被**收紧为 0700** 并记
  warn——里面放私钥，权限过宽不该被当作运维疏忽放过。
- 新建文件 0600、新建目录 0700，权限在**创建时**设定，不留 umask 窗口。
- 所有相对路径经逃逸校验（`..`、`.`、根前缀、盘符一律拒绝）。

### 备份

备份 = 数据目录整体拷贝 + `ACMECAST_CREDENTIAL_KEY`（凭据加密密钥）分开保管。
**只备份数据目录而没有密钥，凭据无法解密**；只换机器不换密钥则直接可用。
Docker 部署时目录挂载到宿主机，注意容器以 uid 65532 运行，目录需对其可写。
