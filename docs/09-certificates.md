# 9. 证书管理

证书由流水线的 `cert.store` 步骤自动入库；本节介绍查询、下载、吊销与手动登记。

## 9.1 证书端点

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/certificates` | 分页列表；`domain` 域名模糊、`sort`=`asc`/`desc`（按**到期时间**）、`page`、`page_size` |
| POST | `/api/certificates` | 手动登记一条证书记录 |
| GET | `/api/certificates/{id}` | 详情 |
| DELETE | `/api/certificates/{id}` | 删除记录（不调用 CA） |
| GET | `/api/certificates/{id}/download` | 按格式下载 |
| POST | `/api/certificates/{id}/revoke` | 吊销 |

列表默认 `sort=asc`（**最快到期的排在最前**），`page_size` 上限 100。
用于仪表盘「即将到期」时，取 `sort=ascending` 的前几条即可。

### 证书响应字段

| 字段 | 说明 |
| --- | --- |
| `id` | 主键 |
| `domains` | 规范化后的域名集合 |
| `cert_pem_path` / `key_pem_path` | 数据目录下的相对路径 |
| `fingerprint` | SHA-256 指纹 |
| `issuer` | 签发者 |
| `not_before` / `not_after` | 生效 / 到期时间 |
| `acme_account_access_id` | 签发所用 ACME 账号凭据 id；手动登记时为 `null` |
| `revoked_at` | 吊销时间；未吊销为 `null` |
| `created_at` / `updated_at` | 记录时间 |

## 9.2 下载格式

`GET /api/certificates/{id}/download?format=<fmt>&password=<pwd>`

| `format` | 内容 | Content-Type | 扩展名 |
| --- | --- | --- | --- |
| `pem`（默认）/`crt` | 证书链 PEM | `application/x-pem-file` | `.pem` |
| `der` | 叶子证书 DER | `application/pkix-cert` | `.der` |
| `pfx` / `p12` | PKCS#12（含私钥） | `application/x-pkcs12` | `.pfx` |
| `jks` | Java KeyStore（含私钥） | `application/x-java-keystore` | `.jks` |
| `p7b` | PKCS#7 证书链 DER | `application/pkcs7-mime` | `.p7b` |

- 转换在内存中完成，不改动已持久化的原始 PEM；
- PFX / JKS 的口令由 `password` 指定；未指定时使用约定默认值 `changeit`；
- JKS 有最小口令长度要求，口令过短会返回错误；
- 下载文件名取域名集合的第一个域名，非法字符会被替换为 `_`；
- 未知格式返回 400 `validation_error`，消息会列出可用格式。

示例：

```bash
# 下载 PEM 证书链
curl -sS -H "Authorization: Bearer $TOKEN" \
  -o example.com.pem \
  "http://127.0.0.1:8080/api/certificates/1/download?format=pem"

# 下载 PFX（指定口令）
curl -sS -H "Authorization: Bearer $TOKEN" \
  -o example.com.pfx \
  "http://127.0.0.1:8080/api/certificates/1/download?format=pfx&password=your-pass"
```

## 9.3 吊销

`POST /api/certificates/{id}/revoke`

请求体可选：

```json
{ "reason": 0 }
```

`reason` 为 RFC 5280 吊销原因代码，缺省时由 CA 决定。可用取值：

| 代码 | 含义 |
| --- | --- |
| 0 | unspecified |
| 1 | keyCompromise |
| 2 | cACompromise |
| 3 | affiliationChanged |
| 4 | superseded |
| 5 | cessationOfOperation |
| 6 | certificateHold |
| 8 | removeFromCRL |
| 9 | privilegeWithdrawn |
| 10 | aACompromise |

响应：

```json
{ "data": { "cert_id": 1, "revoked_now": true, "revoked_at": "2026-09-17T03:00:00Z" } }
```

行为要点：

- 编排顺序为「**先 CA、后本地**」：CA 侧失败时本地状态不变，可重试；
- CA 报「已吊销」（`alreadyRevoked`）视为目的达成，只同步本地状态，
  `revoked_now` 返回 `false`；
- 已吊销的记录再次调用直接返回现状（幂等，不重复调用 CA）；
- 账号取自证书记录上的 `acme_account_access_id`，**不解析流水线配置**；
- 未绑定账号的证书（如手动登记）返回 422 `missing_account`，无法吊销；
- 账号凭据尚未在 CA 侧建立（无 `credentials`）时返回 422 `account_not_established`，
  需先执行一次签发流水线；
- 未配置凭据加密密钥时返回 500 `configuration_error`。

## 9.4 手动登记证书

`POST /api/certificates`

请求体：

```json
{
  "domains": ["example.com"],
  "cert_pem_path": "certs/<fingerprint>/cert.pem",
  "key_pem_path": "certs/<fingerprint>/key.pem",
  "fingerprint": "<sha256>",
  "issuer": "Example CA",
  "not_before": "2026-01-01T00:00:00Z",
  "not_after": "2026-04-01T00:00:00Z",
  "acme_account_access_id": null
}
```

重要说明：

- `cert_pem_path` / `key_pem_path` 是**数据目录下的相对路径**，接口**只登记记录，
  不上传文件、不校验文件是否存在**。请先自行把 PEM 放到数据目录内对应位置；
- 同一域名集合再次登记会**更新**已有记录（而非新增）；
- 手动登记的证书 `acme_account_access_id` 通常为 `null`，因此**无法吊销**；
- 下载所需文件不存在时下载会失败（文件操作错误）。

## 9.5 删除

`DELETE /api/certificates/{id}` 只删除数据库记录，**不删除数据目录中的文件，
也不调用 CA 吊销**。删除后记录的证书文件仍留在磁盘上。

## 9.6 到期状态

系统内部的证书状态分三档（用于判断与告警）：

| 状态 | 条件 |
| --- | --- |
| `healthy` | 距到期尚有余量 |
| `expiring_soon` | 剩余有效期 ≤ 30 天 |
| `expired` | 已过期 |

判定基于**时间点比较**而非「天数四舍五入」，因此刚过期 1 小时的证书会被正确判为
已过期。仪表盘与续期扫描都基于这一判定。
