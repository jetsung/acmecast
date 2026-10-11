//! 腾讯云 SSL 证书库部署。
//!
//! 与 DNS 提供商的测试同一个思路：传输层注入按脚本应答的替身，验证部署
//! 逻辑（查什么、传什么、删什么、失败怎么兜底），全程不碰网络；凭据则走
//! 真实的凭据存储（内存库 + 加密），验证「按标识取出并解密」这条链路。
//! 证书现场生成而非仓库样本——固定样本一旦过期，断言就变成噪声。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use acmecast_access::{CredentialRegistry, CredentialStore, CredentialType};
use acmecast_core::CredentialCipher;
use acmecast_deploy::{
    CertMaterials, DeployMode, DeploymentTarget, TencentSslTarget,
};
use acmecast_dns::{HttpRequest, HttpResponse, HttpTransport};
use acmecast_store::entity::credential;
use acmecast_store::migrate;
use async_trait::async_trait;
use chrono::Utc;
use rcgen::{CertificateParams, DnType, KeyPair, SanType};
use sea_orm::{ActiveModelTrait, Database, Set};
use serde_json::{Value, json};
use schemars::schema::RootSchema;

/// 按脚本依次应答的传输层替身。
#[derive(Debug, Default)]
struct ScriptedTransport {
    responses: Mutex<VecDeque<HttpResponse>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl ScriptedTransport {
    fn new(responses: Vec<HttpResponse>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().expect("锁不应中毒").clone()
    }
}

#[async_trait]
impl HttpTransport for ScriptedTransport {
    async fn send(&self, request: HttpRequest) -> acmecast_dns::Result<HttpResponse> {
        self.requests.lock().expect("锁不应中毒").push(request);

        self.responses
            .lock()
            .expect("锁不应中毒")
            .pop_front()
            .ok_or_else(|| acmecast_dns::Error::provider("脚本里没有更多响应了".to_owned()))
    }
}

/// 一张自签叶子证书的 (证书链 PEM, 私钥 PEM)。
///
/// SAN 首项即主域名——部署目标正是按它去云端找同域名证书的。
fn self_signed_pem(domain: &str) -> (String, String) {
    let mut params = CertificateParams::default();
    params.distinguished_name.push(DnType::CommonName, domain);
    params.subject_alt_names = vec![SanType::DnsName(
        domain.to_owned().try_into().expect("域名应合法"),
    )];
    let key_pair = KeyPair::generate().expect("应能生成密钥");
    let cert = params.self_signed(&key_pair).expect("应能自签");
    (cert.pem(), key_pair.serialize_pem())
}

/// 一份腾讯云凭据字段（默认国内站）。
fn tencent_fields(account_site: Option<&str>) -> Value {
    let mut fields = json!({ "secret_id": "AKIDexample", "secret_key": "secret-key" });
    if let Some(site) = account_site {
        fields["account_site"] = json!(site);
    }
    fields
}

/// 腾讯云体系凭据类型替身（校验与 server 侧同构：两半密钥非空即可）。
#[derive(Debug)]
struct TencentType(&'static str);

impl CredentialType for TencentType {
    fn type_id(&self) -> &'static str {
        self.0
    }

    fn display_name(&self) -> &'static str {
        "腾讯云"
    }

    fn fields_schema(&self) -> RootSchema {
        schemars::schema_for!(Value)
    }

    fn validate(&self, fields: &Value) -> acmecast_core::Result<()> {
        for field in ["secret_id", "secret_key"] {
            if fields[field].as_str().unwrap_or("").trim().is_empty() {
                return Err(acmecast_core::Error::validation(field, "不能为空白"));
            }
        }
        Ok(())
    }
}

/// 其他厂商凭据类型替身（字段任意，只用来验证白名单拒绝）。
#[derive(Debug)]
struct PermissiveType(&'static str);

impl CredentialType for PermissiveType {
    fn type_id(&self) -> &'static str {
        self.0
    }

    fn display_name(&self) -> &'static str {
        "任意字段"
    }

    fn fields_schema(&self) -> RootSchema {
        schemars::schema_for!(Value)
    }

    fn validate(&self, _fields: &Value) -> acmecast_core::Result<()> {
        Ok(())
    }
}

/// 插一份凭据并返回其标识（加密链路与 ssh_deploy 的先例一致）。
async fn seed_credential(
    db: &sea_orm::DatabaseConnection,
    cipher: &CredentialCipher,
    type_id: &str,
    fields: Value,
) -> i64 {
    let sealed = cipher.encrypt_string(&fields.to_string()).expect("应能加密");
    let now = Utc::now();
    credential::ActiveModel {
        name: Set("腾讯云凭据".to_owned()),
        type_id: Set(type_id.to_owned()),
        encrypted_fields: Set(sealed),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("应能插入凭据")
    .id
}

/// 建一次部署环境：内存库 + 注册了凭据类型的存储。
async fn setup() -> (
    &'static sea_orm::DatabaseConnection,
    CredentialStore<'static>,
    Arc<CredentialCipher>,
) {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连上内存库");
    migrate(&db).await.expect("迁移应成功");
    let db: &'static sea_orm::DatabaseConnection = Box::leak(Box::new(db));

    let cipher =
        Arc::new(CredentialCipher::from_base64(&CredentialCipher::generate_key_base64()).unwrap());
    let mut registry = CredentialRegistry::new();
    for type_id in ["cloud.tencent", "dns.tencent-eo"] {
        registry.register(TencentType(type_id)).expect("应能注册");
    }
    registry
        .register(PermissiveType("cloud.cloudflare"))
        .expect("应能注册");
    let store = CredentialStore::new(db, Arc::new(registry), Arc::clone(&cipher));
    (db, store, cipher)
}

fn materials(chain_pem: &str, key_pem: &str) -> CertMaterials {
    CertMaterials::new(chain_pem, key_pem, "sha256:test")
}

// ---- 脚本应答的构造助手 ----

/// `DescribeCertificates` 的一页：`total` 是服务端声称的总数，`certs` 为 (主域名, 证书 ID)。
fn describe_page(total: u64, certs: &[(&str, &str)]) -> HttpResponse {
    let items: Vec<Value> = certs
        .iter()
        .map(|(domain, id)| {
            json!({
                "CertificateId": id,
                "Domain": domain,
                "CertificateType": "SVR",
            })
        })
        .collect();
    HttpResponse::new(
        200,
        json!({"Response": {"TotalCount": total, "Certificates": items, "RequestId": "req"}})
            .to_string(),
    )
}

/// `UploadCertificate` 的成功响应。
fn upload_ok(id: &str) -> HttpResponse {
    HttpResponse::new(
        200,
        json!({"Response": {"CertificateId": id, "RepeatCertId": "", "RequestId": "req"}})
            .to_string(),
    )
}

/// `UploadUpdateCertificateInstance` 的成功响应（原证书 ID 不变、只更新内容）。
fn update_ok() -> HttpResponse {
    HttpResponse::new(
        200,
        json!({"Response": {"RequestId": "req"}}).to_string(),
    )
}

/// `UploadUpdateCertificateInstance` 的失败响应（更新失败）。
fn update_fail(message: &str) -> HttpResponse {
    HttpResponse::new(
        200,
        json!({"Response": {"Error": {"Code": "FailedOperation", "Message": message}, "RequestId": "req"}})
            .to_string(),
    )
}

// ---- 需求：首次上传 ----

#[tokio::test]
async fn a_fresh_certificate_is_uploaded() {
    let (db, store, cipher) = setup().await;
    let id = seed_credential(db, &cipher, "cloud.tencent", tencent_fields(None)).await;

    let (chain, key) = self_signed_pem("example.com");
    let transport = ScriptedTransport::new(vec![describe_page(0, &[]), upload_ok("CERTNEW")]);
    let target = TencentSslTarget::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    let outcome = target
        .deploy(
            &json!({ "credential_id": id, "alias": "acmecast example.com" }),
            &materials(&chain, &key),
            &store,
            DeployMode::Write,
        )
        .await
        .expect("首次部署应成功");

    assert_eq!(outcome.paths, vec!["CERTNEW".to_owned()]);

    let requests = transport.requests();
    assert_eq!(requests.len(), 2, "应先查询再上传");

    assert_eq!(requests[0].url, "https://ssl.tencentcloudapi.com/");
    assert_eq!(requests[0].headers["X-TC-Action"], "DescribeCertificates");
    let payload = requests[0].body.as_ref().expect("查询应有请求体");
    assert_eq!(payload["SearchKey"], "example.com");
    assert_eq!(payload["Limit"], 1000);
    assert_eq!(payload["CertificateType"], "SVR");

    assert_eq!(requests[1].headers["X-TC-Action"], "UploadCertificate");
    let payload = requests[1].body.as_ref().expect("上传应有请求体");
    assert_eq!(payload["CertificatePublicKey"], chain);
    assert_eq!(payload["CertificatePrivateKey"], key);
    assert_eq!(payload["CertificateType"], "SVR");
    assert_eq!(payload["Repeatable"], false);
    assert_eq!(payload["Alias"], "acmecast example.com");
}

#[tokio::test]
async fn the_international_endpoint_serves_intl_credentials() {
    let (db, store, cipher) = setup().await;
    let id = seed_credential(
        db,
        &cipher,
        "cloud.tencent",
        tencent_fields(Some("intl")),
    )
    .await;

    let (chain, key) = self_signed_pem("example.com");
    let transport = ScriptedTransport::new(vec![describe_page(0, &[]), upload_ok("CERTNEW")]);
    let target = TencentSslTarget::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    target
        .deploy(
            &json!({ "credential_id": id }),
            &materials(&chain, &key),
            &store,
            DeployMode::Write,
        )
        .await
        .expect("国际站凭据应能部署");

    for request in transport.requests() {
        assert_eq!(
            request.url, "https://ssl.intl.tencentcloudapi.com/",
            "国际站凭据的全部调用都应发往国际站端点"
        );
    }
}

// ---- 需求：存在即替换 ----

#[tokio::test]
async fn an_existing_certificate_is_replaced() {
    let (db, store, cipher) = setup().await;
    let id = seed_credential(db, &cipher, "cloud.tencent", tencent_fields(None)).await;

    let (chain, key) = self_signed_pem("example.com");
    let transport = ScriptedTransport::new(vec![
        describe_page(1, &[("example.com", "OLD1")]),
        update_ok(),
    ]);
    let target = TencentSslTarget::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    let outcome = target
        .deploy(
            &json!({ "credential_id": id }),
            &materials(&chain, &key),
            &store,
            DeployMode::Write,
        )
        .await
        .expect("替换部署应成功");

    assert_eq!(outcome.paths, vec!["OLD1".to_owned()], "原证书 ID 保持不变");

    let requests = transport.requests();
    assert_eq!(requests.len(), 2, "应按查询、更新的顺序调用");
    assert_eq!(
        requests[1].headers["X-TC-Action"],
        "UploadUpdateCertificateInstance"
    );
    let payload = requests[1].body.as_ref().expect("更新应有请求体");
    assert_eq!(payload["CertificateId"], json!("OLD1"), "更新应携带原证书 ID");
    // UploadUpdateCertificateInstance 的判定：是否真发了这个 Action，而不是
    // 旧的 DeleteCertificates——按 Action 名区分即可，无需断言 Repeatable。
}

#[tokio::test]
async fn wildcard_coverage_is_not_treated_as_existing() {
    let (db, store, cipher) = setup().await;
    let id = seed_credential(db, &cipher, "cloud.tencent", tencent_fields(None)).await;

    // 本次是 a.example.com，云端只有 *.example.com——泛域名覆盖关系不算
    // 「同域名」，不该替人删掉服务所有子域名的泛域名证书。
    let (chain, key) = self_signed_pem("a.example.com");
    let transport = ScriptedTransport::new(vec![
        describe_page(1, &[("*.example.com", "WILD")]),
        upload_ok("CERTNEW"),
    ]);
    let target = TencentSslTarget::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    let outcome = target
        .deploy(
            &json!({ "credential_id": id }),
            &materials(&chain, &key),
            &store,
            DeployMode::Write,
        )
        .await
        .expect("部署应成功");

    assert_eq!(outcome.paths, vec!["CERTNEW".to_owned()]);
    assert_eq!(
        transport.requests().len(),
        2,
        "只应查询与上传，不得发起删除"
    );
}

#[tokio::test]
async fn replace_existing_false_skips_the_lookup_and_the_delete() {
    let (db, store, cipher) = setup().await;
    let id = seed_credential(db, &cipher, "cloud.tencent", tencent_fields(None)).await;

    let (chain, key) = self_signed_pem("example.com");
    // 脚本里只有上传一个应答：若目标在关闭替换后仍发起查询，会因脚本耗尽而报错。
    let transport = ScriptedTransport::new(vec![upload_ok("CERTNEW")]);
    let target = TencentSslTarget::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    let outcome = target
        .deploy(
            &json!({ "credential_id": id, "replace_existing": false }),
            &materials(&chain, &key),
            &store,
            DeployMode::Write,
        )
        .await
        .expect("部署应成功");

    assert_eq!(outcome.paths, vec!["CERTNEW".to_owned()]);
    assert_eq!(transport.requests().len(), 1, "只应上传");
}

// ---- 需求：删除失败的容错 ----

#[tokio::test]
async fn a_failed_update_does_not_fail_the_deployment() {
    let (db, store, cipher) = setup().await;
    let id = seed_credential(db, &cipher, "cloud.tencent", tencent_fields(None)).await;

    let (chain, key) = self_signed_pem("example.com");
    let transport = ScriptedTransport::new(vec![
        describe_page(1, &[("example.com", "OLD1")]),
        update_fail("资源不存在或不可用"),
    ]);
    let target = TencentSslTarget::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    let outcome = target
        .deploy(
            &json!({ "credential_id": id }),
            &materials(&chain, &key),
            &store,
            DeployMode::Write,
        )
        .await
        .expect("单张更新失败只告警、不阻塞整体部署");

    assert_eq!(outcome.paths, vec!["OLD1".to_owned()]);
}

// ---- 需求：跳过写入时不产生云端变更 ----

#[tokio::test]
async fn skip_write_makes_no_requests() {
    let (db, store, cipher) = setup().await;
    let id = seed_credential(db, &cipher, "cloud.tencent", tencent_fields(None)).await;

    let (chain, key) = self_signed_pem("example.com");
    let transport = ScriptedTransport::new(vec![]);
    let target = TencentSslTarget::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    let outcome = target
        .deploy(
            &json!({ "credential_id": id }),
            &materials(&chain, &key),
            &store,
            DeployMode::SkipWrite,
        )
        .await
        .expect("跳过写入应成功");

    assert!(outcome.skipped_write);
    assert!(
        transport.requests().is_empty(),
        "跳过写入不得发起任何云端调用"
    );
}

// ---- 需求：凭据引用与校验 ----

#[tokio::test]
async fn a_non_tencent_credential_is_rejected_without_requests() {
    let (db, store, cipher) = setup().await;
    let id = seed_credential(
        db,
        &cipher,
        "cloud.cloudflare",
        json!({ "api_token": "cf-token" }),
    )
    .await;

    let (chain, key) = self_signed_pem("example.com");
    let transport = ScriptedTransport::new(vec![]);
    let target = TencentSslTarget::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    let error = target
        .deploy(
            &json!({ "credential_id": id }),
            &materials(&chain, &key),
            &store,
            DeployMode::Write,
        )
        .await
        .expect_err("非腾讯云凭据应被拒绝");

    let text = error.to_string();
    assert!(text.contains("可用于 SSL 部署"), "{text}");
    assert!(transport.requests().is_empty(), "被拒绝时不得发起任何请求");
}

#[tokio::test]
async fn the_edgeone_credential_type_is_rejected_without_requests() {
    // EdgeOne 仅服务 DNS、无 SSL 部署能力，tencent-eo 凭据在 SSL 部署中应被拒绝。
    let (db, store, cipher) = setup().await;
    let id = seed_credential(db, &cipher, "dns.tencent-eo", tencent_fields(None)).await;

    let (chain, key) = self_signed_pem("example.com");
    let transport = ScriptedTransport::new(vec![]);
    let target = TencentSslTarget::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    let error = target
        .deploy(
            &json!({ "credential_id": id }),
            &materials(&chain, &key),
            &store,
            DeployMode::Write,
        )
        .await
        .expect_err("tencent-eo 不含 SSL 部署能力，应被拒绝");

    let text = error.to_string();
    assert!(text.contains("可用于 SSL 部署"), "{text}");
    assert!(transport.requests().is_empty(), "被拒绝时不得发起任何请求");
}

// ---- 需求：存在性判断遍历全部分页 ----

#[tokio::test]
async fn the_lookup_paginates_past_the_first_page() {
    let (db, store, cipher) = setup().await;
    let id = seed_credential(db, &cipher, "cloud.tencent", tencent_fields(None)).await;

    let (chain, key) = self_signed_pem("example.com");
    let transport = ScriptedTransport::new(vec![
        // 第一页声称共有 1001 张但没返回同域名证书 → 必须翻页。
        describe_page(1001, &[]),
        describe_page(1001, &[("example.com", "OLD9")]),
        update_ok(),
    ]);
    let target = TencentSslTarget::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    let outcome = target
        .deploy(
            &json!({ "credential_id": id }),
            &materials(&chain, &key),
            &store,
            DeployMode::Write,
        )
        .await
        .expect("分页后的同域名证书应被找到并替换");

    assert_eq!(outcome.paths, vec!["OLD9".to_owned()]);

    let requests = transport.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].body.as_ref().unwrap()["Offset"], 1000);
    assert_eq!(requests[2].headers["X-TC-Action"], "UploadUpdateCertificateInstance");
    let payload = requests[2].body.as_ref().expect("更新应有请求体");
    assert_eq!(payload["CertificateId"], json!("OLD9"));
}

#[tokio::test]
async fn a_full_single_page_is_not_paginated_again() {
    let (db, store, cipher) = setup().await;
    let id = seed_credential(db, &cipher, "cloud.tencent", tencent_fields(None)).await;

    let (chain, key) = self_signed_pem("example.com");
    let transport = ScriptedTransport::new(vec![
        // 总数恰好一页（1000）且本页没有同域名证书 → 取完即止，不空翻一页。
        describe_page(1000, &[]),
        upload_ok("CERTNEW"),
    ]);
    let target = TencentSslTarget::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    let outcome = target
        .deploy(
            &json!({ "credential_id": id }),
            &materials(&chain, &key),
            &store,
            DeployMode::Write,
        )
        .await
        .expect("部署应成功");

    assert_eq!(outcome.paths, vec!["CERTNEW".to_owned()]);
    assert_eq!(transport.requests().len(), 2, "应只查询一次");
}

// ---- 目标声明与 schema ----

#[test]
fn the_target_declares_itself() {
    let target = TencentSslTarget::with_transport(ScriptedTransport::new(vec![]));
    assert_eq!(target.type_id(), "tencent-ssl");
    assert_eq!(target.display_name(), "腾讯云 SSL 证书");

    let example = target.example_input();
    for field in [
        "credential_id",
        "alias",
        "repeatable",
        "replace_existing",
        "resource_types",
        "resource_regions",
    ] {
        assert!(example.get(field).is_some(), "示例应含 {field}");
    }
}

#[test]
fn the_schema_keeps_the_declared_field_order() {
    let target = TencentSslTarget::with_transport(ScriptedTransport::new(vec![]));
    let rendered = serde_json::to_string(&target.input_schema()).unwrap();
    assert!(
        rendered.contains(r#""x-field-order":["credential_id","alias","repeatable","replace_existing","resource_types","resource_regions"]"#),
        "应注入声明顺序: {rendered}"
    );
}

#[test]
fn the_schema_injects_credential_type_allowlist() {
    let target = TencentSslTarget::with_transport(ScriptedTransport::new(vec![]));
    let schema = target.input_schema();
    let properties = schema
        .schema
        .object
        .as_ref()
        .and_then(|object| object.properties.get("credential_id"))
        .expect("应有 credential_id 字段");
    let rendered = serde_json::to_string(&properties).unwrap();
    assert!(
        rendered.contains(r#""x-credential-types":["cloud.tencent"]"#),
        "credential_id 应注入 x-credential-types 仅 cloud.tencent: {rendered}"
    );
    // 新字段进入 properties，受 x-field-order 声明顺序控制。
    assert!(
        schema
            .schema
            .object
            .as_ref()
            .map(|object| object.properties.contains_key("repeatable"))
            .unwrap_or(false),
        "properties 应含 repeatable"
    );
    assert!(
        schema
            .schema
            .object
            .as_ref()
            .map(|object| object.properties.contains_key("resource_types"))
            .unwrap_or(false),
        "properties 应含 resource_types"
    );
    assert!(
        schema
            .schema
            .object
            .as_ref()
            .map(|object| object.properties.contains_key("resource_regions"))
            .unwrap_or(false),
        "properties 应含 resource_regions"
    );
}

#[tokio::test]
async fn repeatable_is_passthrough_to_upload_payload() {
    // repeatable 字段透传到 UploadCertificate 的 Repeatable。
    let (db, store, cipher) = setup().await;
    let id = seed_credential(db, &cipher, "cloud.tencent", tencent_fields(None)).await;

    let (chain, key) = self_signed_pem("example.com");
    let transport = ScriptedTransport::new(vec![describe_page(0, &[]), upload_ok("CERTWITHREP")]);
    let target = TencentSslTarget::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    let outcome = target
        .deploy(
            &json!({ "credential_id": id, "repeatable": true }),
            &materials(&chain, &key),
            &store,
            DeployMode::Write,
        )
        .await
        .expect("带 repeatable 的上传应成功");

    assert_eq!(outcome.paths, vec!["CERTWITHREP".to_owned()]);
    let requests = transport.requests();
    assert_eq!(requests[1].headers["X-TC-Action"], "UploadCertificate");
    let payload = requests[1].body.as_ref().expect("上传应有请求体");
    assert_eq!(payload["Repeatable"], json!(true), "应透传 repeatable=true");
}

#[tokio::test]
async fn resource_types_and_regions_inject_into_update_payload() {
    // resource_types 多选时，更新调用携带 ResourceTypes 为多项，Region 与 Type 笛卡尔积。
    let (db, store, cipher) = setup().await;
    let id = seed_credential(db, &cipher, "cloud.tencent", tencent_fields(None)).await;

    let (chain, key) = self_signed_pem("example.com");
    let transport = ScriptedTransport::new(vec![
        describe_page(1, &[("example.com", "OLD1")]),
        update_ok(),
    ]);
    let target = TencentSslTarget::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    let outcome = target
        .deploy(
            &json!({
                "credential_id": id,
                "resource_types": ["clb", "cdn"],
                "resource_regions": ["ap-guangzhou", "ap-shanghai"]
            }),
            &materials(&chain, &key),
            &store,
            DeployMode::Write,
        )
        .await
        .expect("带多资源类型同步的更新应成功");

    assert_eq!(outcome.paths, vec!["OLD1".to_owned()]);
    let requests = transport.requests();
    assert_eq!(
        requests[1].headers["X-TC-Action"],
        "UploadUpdateCertificateInstance"
    );
    let payload = requests[1].body.as_ref().expect("更新应有请求体");
    assert_eq!(payload["ResourceTypes"], json!(["clb", "cdn"]));
    assert_eq!(
        payload["ResourceTypesRegions"],
        json!([
            { "Region": "ap-guangzhou", "Type": "clb" },
            { "Region": "ap-shanghai", "Type": "clb" },
            { "Region": "ap-guangzhou", "Type": "cdn" },
            { "Region": "ap-shanghai", "Type": "cdn" }
        ])
    );
}

#[tokio::test]
async fn without_resource_types_no_resource_fields_are_carried() {
    let (db, store, cipher) = setup().await;
    let id = seed_credential(db, &cipher, "cloud.tencent", tencent_fields(None)).await;

    let (chain, key) = self_signed_pem("example.com");
    let transport = ScriptedTransport::new(vec![
        describe_page(1, &[("example.com", "OLD1")]),
        update_ok(),
    ]);
    let target = TencentSslTarget::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    target
        .deploy(
            &json!({ "credential_id": id }),
            &materials(&chain, &key),
            &store,
            DeployMode::Write,
        )
        .await
        .expect("默认 resource_types=[] 的更新应成功");

    let requests = transport.requests();
    let payload = requests[1].body.as_ref().expect("更新应有请求体");
    assert!(
        payload.get("ResourceTypes").is_none(),
        "未选资源类型时不应携带 ResourceTypes"
    );
    assert!(
        payload.get("ResourceTypesRegions").is_none(),
        "未选资源类型时不应携带 ResourceTypesRegions"
    );
}

#[tokio::test]
async fn multiple_existing_certificates_are_updated_one_by_one() {
    // 多张匹配逐张更新，单张失败不阻塞整体。
    let (db, store, cipher) = setup().await;
    let id = seed_credential(db, &cipher, "cloud.tencent", tencent_fields(None)).await;

    let (chain, key) = self_signed_pem("example.com");
    let transport = ScriptedTransport::new(vec![
        describe_page(2, &[("example.com", "OLD1"), ("example.com", "OLD2")]),
        update_ok(),
        update_fail("资源不存在"),
    ]);
    let target = TencentSslTarget::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    let outcome = target
        .deploy(
            &json!({ "credential_id": id }),
            &materials(&chain, &key),
            &store,
            DeployMode::Write,
        )
        .await
        .expect("单张更新失败不阻塞整体");

    assert_eq!(
        outcome.paths,
        vec!["OLD1".to_owned(), "OLD2".to_owned()],
        "paths 记录更新的全部证书 ID"
    );
    let requests = transport.requests();
    assert_eq!(requests.len(), 3, "应查询 1 次 + 更新 2 次");
    assert_eq!(requests[1].headers["X-TC-Action"], "UploadUpdateCertificateInstance");
    assert_eq!(requests[2].headers["X-TC-Action"], "UploadUpdateCertificateInstance");
}

// ---- 需求：能力校验 ----
//
// 凭据仅声明 dns 能力（capabilities=["dns"]）时，tencent-ssl 部署在
// 任何云端调用之前以可读输入错误失败；传输层零请求证明没有云端调用。

#[tokio::test]
async fn deploy_fails_without_cloud_requests_when_deploy_capability_is_missing() {
    let (db, store, cipher) = setup().await;
    let sealed = cipher
        .encrypt_string(&tencent_fields(None).to_string())
        .expect("应能加密");
    let now = Utc::now();
    let id = credential::ActiveModel {
        name: Set("仅 DNS 凭据".to_owned()),
        type_id: Set("cloud.tencent".to_owned()),
        encrypted_fields: Set(sealed),
        capabilities: Set(r#"["dns"]"#.to_owned()),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("应能插入凭据")
    .id;

    let (chain, key) = self_signed_pem("example.com");
    // 空脚本：任何云端请求都会因「没有更多响应」报错，若发生即失败暴露。
    let transport = ScriptedTransport::new(vec![]);
    let target = TencentSslTarget::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    let error = target
        .deploy(
            &json!({ "credential_id": id, "alias": "acmecast example.com" }),
            &materials(&chain, &key),
            &store,
            DeployMode::Write,
        )
        .await
        .expect_err("仅 DNS 能力的凭据部署应失败");

    let message = error.to_string();
    assert!(
        message.contains(&id.to_string()) && message.contains("部署"),
        "错误应提示凭据标识与缺失能力：{message}"
    );
    assert!(
        transport.requests().is_empty(),
        "能力缺失时不得发起任何云端请求"
    );
}
