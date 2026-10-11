# 证书管理

## 入库与去重

证书入库（`cert.store` 步骤或 `POST /api/certificates` 手动上传）按**域名
集合**去重：同一域名集合再次入库是更新已有记录而非新增，续期重跑不会产生
重复记录。证书链 PEM 与私钥 PEM 落盘到数据目录（见
[数据库与数据目录](11-database.md)），库中存相对路径与元数据（指纹、签发者、
起止时间、签发账号凭据标识）。

指纹 = 整个叶子证书 DER 的 SHA-256 小写 hex，作为文件目录名与幂等依据。
域名取自 SAN（保序、含 `*.`）；SAN 缺失时回退 Subject CN。

## 到期状态

三态，全局唯一实现（store 实体也委托同一函数，不会两处漂移）：

| 状态 | 判定 |
|---|---|
| `healthy` | `not_after > now + 阈值` |
| `expiring_soon` | `not_after <= now + 阈值`（恰好等于阈值也算临期；`now == not_after` 瞬间按临期而非过期） |
| `expired` | `now > not_after` |

阈值默认 30 天（续期扫描同一阈值）。判定比较**时间点**而非天数——
「剩余 0 天」可能实际已过期 23 小时。

## 下载格式

`GET /api/certificates/{id}/download?format=...&password=...`：

| format | 别名 | Content-Type | 说明 |
|---|---|---|---|
| `pem`（默认） | `crt` | `application/x-pem-file` | 证书链 PEM |
| `der` | — | `application/pkix-cert` | 叶子证书 DER |
| `pfx` | `p12` | `application/x-pkcs12` | PKCS#12，默认 AES-256 加密（PBES2 + HMAC-SHA256）；口令默认 `changeit`，alias `acmecast` |
| `jks` | — | `application/x-java-keystore` | JKS，口令最短 6 位（Java 自身约束）；alias 统一转小写 |
| `p7b` | — | `application/pkcs7-mime` | PKCS#7 certs-only |
| `key` | `private-key`、`key-pem`（大小写不敏感） | `application/x-pem-file` | 私钥 PEM（原样返回新布局 `key`，不做格式转换，文件名 `*.key`） |

转换全部在内存中完成（纯 Rust，无 OpenSSL 依赖），不改持久化的 PEM（私钥 `key` 分支原样返回，不经转换）。
下载文件名取首个域名（仅保留字母数字与 `.`、`-`、`_`），私钥分支追加 `.key`。

## 详情页展示

证书详情页（`/certificates/{id}`）展示「私钥算法」（`key_algorithm`，如 `ECDSA P-256`、`RSA`，历史数据可为空时显示 `—`）与「描述」（`description`，空白归一为 `—`，长文本截断并悬浮查看完整内容），并提供「下载私钥 (PEM)」按钮（`format=key`，与多格式证书下载并存，失败按现有错误通道提示）。
## 吊销

`POST /api/certificates/{id}/revoke`，可选 `reason`（RFC 5280 原因码 0–10，
如 `1`=keyCompromise、`4`=superseded）：

1. 先用签发时记录的 ACME 账号向 CA 吊销；
2. CA 报「已吊销」→ 只同步本地状态（`revoked_now=false`）；
3. 本地标记吊销时间，并把证书文件**归档**到 `certs/revoked/<指纹>/`
   （只换目录不改名；源文件缺失时保持库中路径不变）。

已吊销的记录重复吊销幂等直接返回；手动上传（无签发账号）的记录返回
422 `missing_account`——请去对应 CA 的管理渠道吊销。

## 手动归档

`POST /api/certificates/{id}/archive`：将活跃目录 `certs/ssl/<主域名>.*`
移至归档目录 `certs/archived/<指纹>/`（按该证书记录的指纹分子目录，文件名
不变），并同步更新数据库记录的 `cert`/`ca`/`fullchain`/`key`（兼容旧 `cert_pem_path`/`key_pem_path`）。归档为本地
文件移动，不触及吊销状态与 CA，证书详情与下载（`format=pem|key`）仍从新
路径读取。

- 已在 `certs/archived/<指纹>/` 或 `certs/revoked/<指纹>/` 的再次归档幂等
  返回成功（`archived_now=false`）。
- 源文件缺失时幂等返回成功并记 `warn!`，库路径保持不变（与吊销归档的降级
  一致）。
- IO 失败返回错误，文件保留在原位，库路径不变。
- 归档目录与文件权限分别为 `0700` / `0600`，与现有归档/吊销目录一致，路径
  含逃逸字符时拒绝。
- 已归档证书仍可吊销：吊销时从 `certs/archived/<指纹>/` 读取证书 PEM，
  成功后进一步移动到 `certs/revoked/<指纹>/`。

证书列表页（`/certificates`）在“操作”列内提供“编辑 / 吊销 / 归档”
三项行内操作，均需 `stopPropagation` 防行跳转；“吊销”“归档”均需二次确认。
“吊销”沿用 `POST /api/certificates/{id}/revoke`，成功后刷新列表并提示
“证书已吊销，本地状态已更新”；“归档”调用 `POST /api/certificates/{id}/archive`，
成功后刷新并提示“证书已归档”。互斥规则：**吊销为最终态**——`revoked_at` 非空
时“吊销”与“归档”均禁用；**已归档不阻断吊销**——`certs/archived/<指纹>/` 的证书
仍可吊销，吊销成功后进一步移至 `certs/revoked/<指纹>/`。

## 私钥校验

入库/上传时校验私钥与证书匹配：比较私钥推导的公钥与证书 SPKI 的
subjectPublicKey 字节。私钥仅支持 **PKCS#8**（`BEGIN PRIVATE KEY`）；
PKCS#1（`BEGIN RSA PRIVATE KEY`）会报错提示先转换：

```bash
openssl pkcs8 -topk8 -nocrypt -in rsa.key -out pkcs8.key
```

支持的算法：RSA、ECDSA P-256/P-384、Ed25519。
