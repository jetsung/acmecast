# DNS 提供商与传播检查

DNS-01 挑战的生命周期：**写入 TXT 记录 → 等传播 → 通知 CA 验证 →
无论成败都清理记录**。记录名为 `_acme-challenge.<域名>`（通配符去掉 `*.`），
记录值为 key authorization 的 SHA-256 摘要（base64url 无填充）。

## 内置提供商

| type_id | 展示名 | 凭据字段 | 备注 |
|---|---|---|---|
| `cloudflare` | Cloudflare | `api_token` | V4 REST API，Bearer 认证；先按 zone 换 zone_id；删除只删值相同的记录 |
| `aliyun` | 阿里云 DNS | `access_key_id`、`access_key_secret` | RPC 风格 API（HMAC-SHA1 签名）；TTL 被 clamp 到 600–86400（低于 600 阿里云直接 400） |
| `tencent` | 腾讯云 DNSPod | `secret_id`、`secret_key`、`account_site` | API 3.0（TC3-HMAC-SHA256 签名）；`account_site` 选 `cn`/`intl`（国内/国际站）；TTL 下限 600（免费/标准版报 `LimitExceeded.RecordTtlLimit`） |
| `tencent-eo` | 腾讯云 EdgeOne | `secret_id`、`secret_key`、`account_site` | 共用腾讯云 API 3.0 签名基础，换 EdgeOne 服务名与 Action；`DescribeZones` 查 zone 是否已接入 EdgeOne |

通用行为：

- 凭据反序列化前对所有字符串字段 **trim 首尾空白**（粘贴带换行不致认证失败）。
- `create_txt` / `delete_txt` 幂等：记录本不存在视为成功。
- zone 由调用方显式给出，不从记录名推导（公共后缀如 `.co.uk` 会推错）。

## 写入与清理语义

`cert.apply` 对每个授权域名执行：

1. 写前先查：同值记录已存在 → 幂等跳过；**异值残留 → 直接报错**（防止 CA
   读到旧值验签失败）。
2. 写入 TXT 记录。
3. 等传播（见下，可用 `wait_propagation=false` 关闭）。
4. 通知 CA 挑战就绪。
5. **无论成败都清理**：action 失败报原始错误（清理失败只 warn）；成功但清理
   失败则优先报清理错误。

## 传播等待

默认策略：先向 **provider 侧**确认记录确实写入，再检查解析器可见性。
默认间隔 5 秒、总超时 120 秒；单次解析器查询上限 5 秒。

### 权威 NS 直查（默认）

**默认不走公共 DoH**，而是直查权威 NS：

1. 经可信公共引导 DNS（阿里 223.5.5.5、腾讯 119.29.29.29、114、百度、
   1.1.1.1、8.8.8.8、9.9.9.9 等，仅一跳）查出 zone 的 NS 委派及其 A/AAAA。
2. 对每台 NS 直接查 TXT/CNAME。

判定规则：

- **全部应答 NS 一致**才算可见（anycast POP 有同步延迟，防 CA 打到滞后 POP
  吃 NXDOMAIN）；单台失败跳过，全部失败才报错。
- NXDOMAIN/NODATA 是合法的「还没有」，继续等。
- CNAME 也只信权威直查：全部 NS 给出同一 CNAME 才跟随（支持挑战记录独立
  托管到别的 zone）。

不走公共 DoH 的原因：公共解析器的 NXDOMAIN 负缓存（SOA minimum 通常
600–1800 秒）会让「写入 → 删除 → 重试」的第二次尝试稳定查不到新值。

### 提供商感知的 DoH 排序

传播等待的分阶段回退（阶段 2/3）使用全量 DNS + DoH 集合，按提供商标识感知排序：首选提供商对应的同厂商 DoH 端点排第一，其余按固定序兜底。排序规则：

| 提供商 | 首选 DoH | 兜底序 |
|---|---|---|
| `cloudflare` | Cloudflare | Cloudflare → 阿里 → 腾讯 → Google |
| `aliyun` | 阿里 | 阿里 → Cloudflare → 腾讯 → Google |
| `tencent` / `tencent-eo` | 腾讯 | 腾讯 → Cloudflare → 阿里 → Google |
| 其他 / 未传参 | — | Cloudflare → 阿里 → 腾讯 → Google |

"任意一家可见即就绪"的判定语义不变——`probe_resolvers` 逐个查，命中即返回。`ACMECAST_DOH_RESOLVERS` 显式设置时提供商感知分支不生效，按环境变量语义构造集合（`none` 即跳过解析器阶段）。

### 自定义解析器（腾讯云手动配置）

显式设置后改用配置而非权威直查。腾讯云用户在国内环境建议追加腾讯 DoH
以提升可达性，`data/config.toml` 与 `acmecast-server config init` 模板已附
可直接取消注释的片段：

```toml
[[resolvers]]
type = "doh"
endpoint = "https://doh.pub/dns-query"
name = "腾讯 DNSPod"

[[resolvers]]
type = "doh"
endpoint = "https://1.12.12.12/dns-query"
name = "腾讯 DNSPod（IP 直连）"

# 传统 UDP 53（目前仅校验，传输层未实现；配置后告警跳过，留作清单备用）
# [[resolvers]]
# type = "dns"
# endpoint = "119.29.29.29"
# name = "腾讯 DNSPod"
# [[resolvers]]
# type = "dns"
# endpoint = "119.28.28.28"
# name = "腾讯 DNSPod"
```

环境变量等价写法：

```bash
export ACMECAST_DOH_RESOLVERS="腾讯 DoH=https://doh.pub/dns-query,https://1.12.12.12/dns-query"
# 跳过传播等待（不推荐）：export ACMECAST_DOH_RESOLVERS=none
```

- 环境变量 `ACMECAST_DOH_RESOLVERS`：逗号分隔的 `名称=端点`（或仅端点）；
  **`none` = 跳过传播等待**（provider 确认写入后立即让 CA 验证，不推荐）。
- config.toml 的 `[[resolvers]]` 段，见[配置参考](03-configuration.md)。
- 合并顺序：内置 → config 条目 → 环境变量，按归一化端点去重。

此模式下**任一**解析器看到记录即算就绪（要求全部看到会被最慢/不可达的
拖死）；查询失败按「不可达」归并，不参与判定。

超时错误会区分两种形态：「提供商侧没认」与「解析器没看到/不可达」，排障时
先确认是哪种。

## 内置公共 DNS 清单

- **UDP 53**：223.5.5.5、223.6.6.6、119.29.29.29、114.114.114.114、
  114.114.115.115、180.76.76.76、1.1.1.1、8.8.8.8、9.9.9.9、149.112.112.112
  （含 `dnsdata/dns_servers.txt` 中收录的腾讯 `119.28.28.28` / `119.29.29.29` 体系）
- **DoH**：dns.alidns.com、doh.pub、1.12.12.12、120.53.53.53、223.5.5.5、
  dns.google、cloudflare-dns.com、1.1.1.1（均 `/dns-query`）
  （其中 `doh.pub` / `1.12.12.12` / `120.53.53.53` 为腾讯 DNSPod 公共 DoH）

> 腾讯 DoH 端点说明：`https://doh.pub/dns-query` 与 `https://1.12.12.12/dns-query`
> / `https://120.53.53.53/dns-query` 同源（DNSPod），前者走域名便于证书校验，
> 后者走 IP 直连在 DNS 劫持环境更稳；按网络环境二选一或并存均可。

## HTTP-01 的现状

HTTP-01 目前只有**材料计算**能力（token、`/.well-known/acme-challenge/<token>`
路径、key authorization 响应体），没有投放通道。申请证书请使用 DNS-01。

## 通配符与 CNAME 的排障

### 泛解析（`*.<zone>`）不要把 `_acme-challenge` 也套进去

最常见的误用：为了把 `*.example.com` 全指向一台服务器，配了

```
*.example.com.  CNAME  oversea.example.net.
```

`_acme-challenge.example.com` 上**没有显式节点**时，按 `RFC 4592` 它会被上面的通配 `CNAME` **合成**出来：

```
_acme-challenge.example.com.  CNAME  oversea.example.net.
```

此后对 `_acme-challenge.example.com` 查 `TXT`，权威 NS 回的是 `CNAME`，`DNS-01` 永远拿不到 `TXT`，日志可能是 `已在权威/公共解析器可见`（本机 DoH 命中 CNAME 的 `NOERROR`）但 CA `secondary validation: No TXT record found`。

**修复：**

1. 不要给 `_acme-challenge` 建显式的 `CNAME`，只保留 `*.example.com` 的那条。
2. `acmecast` 往原区写入 `_acme-challenge.example.com TXT` 成功后，该名字上就有了**显式节点**，按 `RFC 4592` 它会**压掉通配**，再查 `TXT` 就只回 `TXT`、不回 `CNAME` 了。验证时请直查权威 NS：

   ```
   dig @权威NS TXT _acme-challenge.example.com
   ```

   确认只回 `TXT` 不回 `CNAME` 后再触发流水线。
3. 若想彻底不纠结，把泛解析从 `CNAME` 改成 `A` 到目标 IP，业务效果等同，且 `_acme-challenge` 天然不受影响。

### `CNAME` 与 `TXT` 同名互斥

`CNAME` 的语义是“此名别处定”（RFC 1034），同名处**不允许**再有 `TXT`。已有 `CNAME` 时，往同名写 `TXT` 要么被 `DNS` 服务商拒绝，要么写入成功但递归查询时 `CNAME` 会**遮蔽** `TXT` —— `acmecast` 的权威直查会同时取 `TXT + CNAME`，`CNAME` 存在时按 `CA` 的跟随语义跟到目标区（[authoritative.rs:104](../crates/acmecast-dns/src/authoritative.rs#L104)），原区 `TXT` 对验证者不可见。

**规则：** 挑战委托（`_acme-challenge.x CNAME _acme-challenge.y.`）是允许的，但它要求**目标区**上能查到 `TXT`；**业务泛 `CNAME` 与挑战 `TXT` 混在同名上是不允许的**。[故障排查](15-troubleshooting.md)有 `CNAME` 遮蔽的最小复现与 `dig` 排查步骤。
