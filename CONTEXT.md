# AI 协作笔记

> 给 AI 助手的实现思路参考(项目根目录,不进用户文档站)。下次遇到同类需求时,
> 让 AI 先读本文件再动手。当前篇目:

## Schema 驱动表单:布局扩展与实现思路

> 凭据页、流水线表单的**字段布局、控件形态、联动行为**都是后端 schema 驱动的。
> 遇到「把某字段放哪一行 / 换成什么控件 / 什么时候必填」这类需求时,先读决策表与
> 踩坑清单再动代码——**通常不需要改前端渲染器**。

## 一句话架构

```
Rust 结构体 (derive schemars::JsonSchema)          ← 字段定义的唯一事实源
    │  schema_for!(T) 产出 JSON Schema(可再注入 x-* 扩展)
    ▼
/api/credential-types · /api/tasks/{type_id}/schema
    │  前端按 type_id 拉取
    ▼
SchemaForm.tsx 通用渲染器(凭据页与流水线页共用)
    ├─ 控件选择:string→输入框、enum→下拉、x-multiline→textarea、
    │            boolean→Switch、array<object>→JSON 文本
    ├─ 布局宽度:默认标量半宽(12)两列一行;标注可改整行(24)或行尾换行
    └─ 联动:x-required-when / x-visible-when / x-field-order / x-options
```

**推论**:改「某字段占几列、是否 textarea、何时必填」= 改后端 schema(注入扩展键),
前端渲染器是通用引擎,不要在它里面写任何具体类型的分支。

## 布局决策表(需求 → 扩展 → 注入位置)

| 需求效果 | 用哪个扩展 | 控件宽度 | 注入位置 |
|---|---|---|---|
| 两个短字段排同一行(如 `cert_path`/`key_path`) | 不标注(默认流式) | 半宽 | — |
| 字段独占一行,**框撑满**整行 | `x-full-width: true` | 满行 | 后端或调用方 |
| 多行文本(私钥 PEM、含私钥的 JSON) | `x-multiline: true` | 满行(textarea) | 后端 schema 注入 |
| 字段独占一行,**框只占半列**,右侧留空 | `x-end-row: true`(配半宽) | 半宽 + 行尾空占位列 | 后端 schema 注入 |
| 字段渲染顺序(对抗字母序重排) | 顶层 `x-field-order: [...]` | — | 后端 schema 注入 |
| 条件必填(如 `ca=custom` 时 URL 必填) | 字段上 `x-required-when` | — | 后端 schema 注入 |
| 条件显隐 | 字段上 `x-visible-when` | — | 后端 schema 注入 |
| 资源 ID 选名称(凭据下拉) | 调用方注入 `x-options` | — | 前端调用方 |
| 部署 config 随 target 变结构 | `x-target-schemas` | 满行 | 后端步骤 schema |

> ⚠️ **最容易搞混的一对**:「独占一行但框只要半列」用 **`x-end-row`**,不是 `x-full-width`——
> 后者会把控件撑满两列(antd 控件默认 `width: 100%`)。`x-end-row` 的语义是「半宽照旧,
> 渲染后补一个空占位列把后续字段推到新行」。

## x-* 扩展速查(实现位置)

| 扩展 | 语义 | 顶层分支 | 子表单分支(ObjectFieldGroup) |
|---|---|---|---|
| `x-multiline` | string 渲染 textarea | ✅ span 24 | ✅ span 24 |
| `x-full-width` | 标量占整行 | ✅ span 24 | ✅ span 24 |
| `x-end-row` | 半宽 + 行尾空占位列 | ✅(本次补齐) | ✅ 既有 |
| `x-field-order` | 钉字段渲染顺序 | ✅ `orderedEntries` | —(子字段按对象键序) |
| `x-required-when` | 条件必填(`Equals`/`MinItems`) | ✅ | ✅ |
| `x-visible-when` | 条件显隐(`Equals`) | ✅ | ✅ |
| `x-target-schemas` | 变体结构化子表单 | ✅ | — |

分支位置:`frontend/src/components/SchemaForm.tsx`——顶层表单(`SchemaForm` 主组件,
凭据页与 cert.apply 用)与部署目标子表单(`ObjectFieldGroup`,`cert.deploy` 的 config 用)。
**给某分支加能力时,确认另一个分支是否也需要**(本次 `x-end-row` 就是只在子分支有、
凭据页踩空)。

## 实操配方:给一组凭据字段排布局(以腾讯云为例)

后端注入(Rust,`crates/acmecast-dns/src/providers/tencent.rs`):

```rust
/// serde_json 的 Map 会把 schemars 产物的 properties 按字母序重排——
/// 用 x-field-order 钉住渲染顺序;x-end-row 让站点独占一行但框半宽。
#[must_use]
pub fn tencent_credentials_schema() -> RootSchema {
    let mut schema = schema_for!(TencentCredentials);
    let Some(object) = schema.schema.object.as_mut() else {
        return schema;
    };
    schema.schema.extensions.insert(
        "x-field-order".to_owned(),
        serde_json::json!(["account_site", "secret_id", "secret_key"]),
    );
    if let Some(schemars::schema::Schema::Object(site)) = object.properties.get_mut("account_site") {
        site.extensions.insert("x-end-row".to_owned(), serde_json::Value::Bool(true));
    }
    schema
}
```

凭据类型接线(`crates/acmecast-server/src/credentials.rs`):`fields_schema()` 返回
上述函数(而不是裸 `schema_for!`)。

效果:

```
account_site (1/2 列)  [空占位列]
secret_id (1/2 列)  secret_key (1/2 列)
```

## 踩坑清单(本次实战教训)

1. **properties 字母序重排**:serde_json 的 `Map` 默认按字母序存储,schemars 产物的
   `properties` 顺序 ≠ Rust 结构体声明顺序(如 `account_site` 会抢到 `secret_id` 前面)。
   布局依赖顺序时**必须**注入 `x-field-order`,否则「谁和谁同行」不可控。
2. **「独占一行」的两种含义**:
   - 框撑满两列 → `x-full-width`(span 24);
   - 框保持半列、右侧留空 → 半宽 + `x-end-row`(行尾空占位列 `aria-hidden`)。
   需求方说「单独一行」时先问清(或看参照页面)是哪种,流水线部署配置里的
   「SSH 主机档案」是后者。
3. **schemars 的 `allOf[$ref]` 包装**:字段类型是具名枚举/结构体时,产物形如
   `{"description":…, "allOf":[{"$ref":"#/definitions/T"}]}`。扩展键注入在 **wrapper** 上
   即可——前端 `deref` 会把外层键合并回解包后的节点,span/联动计算都能读到。
4. **顶层分支与子表单分支能力不对齐**:同一扩展可能在两个分支各自实现。新增/使用前
   `grep "x-xxx" SchemaForm.tsx` 确认两条路径都支持。
5. **测试断言 ant-col 的姿势**:输入元素 `closest(".ant-col")` 会命中 Form.Item 内层的
   控制列——要先 `closest(".ant-form-item")` 再向外 `closest(".ant-col")`。凭据弹窗里
   断言 label 顺序时要过滤掉「名称 / 类型」两个外层表单项。
6. **真机才暴露的服务端行为**(伪传输层测试测不出,发布后留观察点):
   - DNSPod 免费/标准版 TTL 下限 600(报 `LimitExceeded.RecordTtlLimit`)→ 提供商侧钳制;
   - 腾讯云 API 3.0 业务错误也回 HTTP 200(错误在 `Response.Error`)→ 先看业务错误再看状态码;
   - Cloudflare `success: false` 也可能出现在 200 响应里 → 同上。
7. **凭据连通性测试的分层**:凭据类型(`CredentialType::test_connectivity`)只做编排与
   脱敏,探测动作是提供商的固有方法(`verify_credentials`,不进 `DnsProvider` trait);
   用轻量身份接口(Cloudflare `/user/tokens/verify`、各家 STS `GetCallerIdentity`),
   不要求业务权限;错误先过 `redact` 再呈现。

## 验证路径

```bash
cargo test -p acmecast-dns -p acmecast-server   # 后端(schema 注入、探测)
cargo clippy --workspace                         # 零新增告警
cd frontend && npx vitest run                    # SchemaForm.test / CredentialListPage.test / PipelineEditorPage.test
openspec validate <change> --strict              # 规划工件校验
```

改了后端 schema 要**重启 `just dev`**(`cargo run` 不热载);前端 vite 热更新即时生效。

## 本次变更的文件索引(credential-form-layout-and-dns-test)

- 后端 schema 注入:`crates/acmecast-dns/src/providers/tencent.rs`(`tencent_credentials_schema`)、
  `crates/acmecast-access/src/credential/acme_account.rs`(`credentials`→`x-multiline`、
  `directory_url`→`x-full-width`)
- 凭据探测:`crates/acmecast-dns/src/providers/{cloudflare,aliyun,tencent,tencent_eo}.rs`
  (`verify_credentials`)+ `crates/acmecast-server/src/credentials.rs`(四个凭据类型接线,
  `with_transport` 注入替身供测试)
- 前端渲染器:`frontend/src/components/SchemaForm.tsx`(顶层分支补 `x-full-width` 与 `x-end-row`)
- 测试:`crates/acmecast-dns/tests/providers.rs`、`frontend/src/components/SchemaForm.test.tsx`、
  `frontend/src/pages/credentials/CredentialListPage.test.tsx`
