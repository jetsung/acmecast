//! 11.1 端到端链路：ACME 申请 → 证书入库 → 部署到本地目录。
//!
//! CA 用本地 [pebble](https://github.com/letsencrypt/pebble)（Let's Encrypt
//! 的 ACME 测试实现），`PEBBLE_VA_ALWAYS_VALID=1` 让挑战验证直接通过——
//! 端到端要验证的是**链路本身**（下单、挑战材料、CSR、签发、入库、部署），
//! 不是 CA 的验证器。DNS 提供商用内存替身，记录写入与清理都会被断言。
//!
//! 已有运行中的 pebble 时可设 `ACMECAST_PEBBLE_DIR` 直连；否则测试自己
//! 起一个容器并在结束时清理。

mod support;

use std::collections::BTreeMap;
use std::process::Command;
use std::sync::{Arc, Mutex};

use acmecast_access::{CredentialRegistry, CredentialStore, CredentialType};
use acmecast_core::CredentialCipher;
use acmecast_deploy::DeploymentRegistry;
use acmecast_deploy::state::InMemoryDeploymentState;
use acmecast_dns::{DnsProvider, DnsProviderRegistry, TxtRecord, challenge_record_name};
use acmecast_pipeline::{PipelineDefinition, PipelineRunner, StepRegistry};
use acmecast_scheduler::PipelineLauncher as _;
use acmecast_server::steps::{CertApplyStep, CertDeployStep, CertStoreStep};
use acmecast_store::migrate;
use acmecast_store::repository::{CertInput, CertQuery, CertRepository};
use async_trait::async_trait;
use axum::http::StatusCode;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Database, DatabaseConnection, EntityTrait, QueryFilter, Set,
};
use support::*;

// ---- 内存 DNS 提供商：记录写入与清理，供链路断言 ----

#[derive(Debug, Default, Clone)]
struct MemoryDns {
    records: Arc<Mutex<BTreeMap<String, Vec<String>>>>,
}

impl MemoryDns {
    fn snapshot(&self) -> BTreeMap<String, Vec<String>> {
        self.records.lock().expect("测试锁不会中毒").clone()
    }
}

#[async_trait]
impl DnsProvider for MemoryDns {
    fn type_id(&self) -> &'static str {
        "memory"
    }

    fn display_name(&self) -> &'static str {
        "内存测试提供商"
    }

    fn credential_fields(&self) -> schemars::schema::RootSchema {
        schemars::schema_for!(MemoryDnsFields)
    }

    async fn find_txt(
        &self,
        _credentials: &serde_json::Value,
        record: &TxtRecord,
    ) -> acmecast_dns::Result<Vec<String>> {
        Ok(self
            .records
            .lock()
            .expect("测试锁不会中毒")
            .get(&record.name)
            .cloned()
            .unwrap_or_default())
    }

    async fn create_txt(
        &self,
        _credentials: &serde_json::Value,
        record: &TxtRecord,
    ) -> acmecast_dns::Result<()> {
        self.records
            .lock()
            .expect("测试锁不会中毒")
            .entry(record.name.clone())
            .or_default()
            .push(record.value.clone());
        Ok(())
    }

    async fn delete_txt(
        &self,
        _credentials: &serde_json::Value,
        record: &TxtRecord,
    ) -> acmecast_dns::Result<()> {
        if let Some(values) = self
            .records
            .lock()
            .expect("测试锁不会中毒")
            .get_mut(&record.name)
        {
            values.retain(|value| value != &record.value);
        }
        Ok(())
    }
}

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
struct MemoryDnsFields {}

/// 让内存提供商满足「带凭据类型」的注册要求（它不需要真凭据）。
#[derive(Debug)]
struct MemoryDnsType;

#[async_trait]
impl CredentialType for MemoryDnsType {
    fn type_id(&self) -> &'static str {
        "dns.memory"
    }

    fn display_name(&self) -> &'static str {
        "内存 DNS"
    }

    fn fields_schema(&self) -> schemars::schema::RootSchema {
        schemars::schema_for!(MemoryDnsFields)
    }

    fn validate(&self, _fields: &serde_json::Value) -> acmecast_core::Result<()> {
        Ok(())
    }
}

// ---- pebble 容器管理 ----

/// pebble 容器的生命周期守卫：析构时强制删除容器。
struct Pebble {
    container_id: String,
    directory_url: String,
}

impl Drop for Pebble {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.container_id])
            .output();
    }
}

/// 挑一个宿主机端口给 pebble 的 `-p` 用。
///
/// 不能让内核随便给（`bind("127.0.0.1:0")`）：那样拿到的端口落在
/// `ip_local_port_range`（Linux 默认 32768–60999）里，而 rootless Docker 把容器端口
/// 发布到该区间会失败——实测 25000 可连、35000 / 45000 拒连。端口连不上时 `/dir`
/// 探测会空转到超时，用例静默跳过，看起来还是绿的。这里只在临时端口区间之下试探。
///
/// 起点取随机值，避免并行跑的多个用例都从同一个端口开始扫、互相抢同一个。
fn pick_host_port() -> Result<u16, String> {
    // 与容器内的 14000 错开，便于在 `docker ps` 里一眼认出来；
    // 32000 低于 Linux 的 32768 与 macOS 的 49152，两边都不会被内核临时占用。
    const START: u16 = 14001;
    const END: u16 = 32000;
    const SPAN: u16 = END - START;

    let seed = uuid::Uuid::new_v4().into_bytes();
    let offset = u16::from_be_bytes([seed[0], seed[1]]) % SPAN;
    (0..SPAN)
        .find_map(|step| {
            let port = START + (offset + step) % SPAN;
            // 绑 0.0.0.0 是因为 docker 的 `-p {port}:14000` 也发布在 0.0.0.0：
            // 只探 127.0.0.1 会漏掉「loopback 空闲但通播地址已被占」的端口。
            // 探完即释放，交给 docker 去绑（与原来一样存在这点竞态）。
            std::net::TcpListener::bind(("0.0.0.0", port))
                .ok()
                .map(|_| port)
        })
        .ok_or_else(|| format!("{START}–{END} 内找不到空闲端口"))
}

fn start_pebble() -> Result<Pebble, String> {
    if let Ok(directory_url) = std::env::var("ACMECAST_PEBBLE_DIR") {
        return Ok(Pebble {
            container_id: String::new(),
            directory_url,
        });
    }

    let host_port = pick_host_port()?;

    let output = Command::new("docker")
        .args([
            "run",
            "-d",
            "--rm",
            "-e",
            "PEBBLE_VA_ALWAYS_VALID=1",
            "-p",
            &format!("{host_port}:14000"),
            "ghcr.io/letsencrypt/pebble:latest",
        ])
        .output()
        .map_err(|e| format!("启动 pebble 容器失败（需要 Docker）: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "pebble 容器启动失败: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let container_id = String::from_utf8_lossy(&output.stdout).trim().to_owned();

    // 先接住守卫再等就绪。下面超时返回 Err 时守卫会随局部变量一起析构，
    // 容器不会被留在后台——否则每次都漏一个容器，而且测试还报 ok。
    let pebble = Pebble {
        container_id,
        directory_url: format!("https://127.0.0.1:{host_port}/dir"),
    };

    // 等目录端点就绪：手写一个最小 HTTP GET，避免为此启用 reqwest 的
    // blocking feature——tokio 运行时里起阻塞线程反而绕。
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if directory_ready(&pebble.directory_url) {
            return Ok(pebble);
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    Err("pebble 目录端点在 30 秒内未就绪".to_owned())
}

/// 探测目录端点；pebble 用自签 TLS，必须 `-k` 跳过校验。
fn directory_ready(url: &str) -> bool {
    Command::new("curl")
        .args(["-ksS", "--max-time", "3", url])
        .output()
        .map(|output| {
            let body = String::from_utf8_lossy(&output.stdout);
            output.status.success() && body.contains("newNonce")
        })
        .unwrap_or(false)
}

// ---- 测试 ----

async fn a_database() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连上内存库");
    migrate(&db).await.expect("迁移应成功");
    db
}

/// 预置一份 ACME 账号凭据（未注册账号——链路要验证首次注册与写回）。
async fn an_acme_account_credential(
    db: &DatabaseConnection,
    cipher: &CredentialCipher,
    directory_url: &str,
) -> i64 {
    let fields = serde_json::json!({
        "ca": "custom",
        "directory_url": directory_url,
    });
    let encrypted = cipher
        .encrypt_string(&fields.to_string())
        .expect("应能加密凭据");
    let row = acmecast_store::entity::credential::ActiveModel {
        name: Set("pebble 账号".to_owned()),
        type_id: Set("acme.account".to_owned()),
        encrypted_fields: Set(encrypted),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("凭据应能写入");
    row.id
}

#[tokio::test(flavor = "multi_thread")]
async fn the_full_pipeline_applies_stores_and_deploys_a_certificate() {
    let pebble = match start_pebble() {
        Ok(pebble) => pebble,
        Err(reason) => {
            eprintln!("跳过端到端测试：{reason}");
            return;
        }
    };

    let db = a_database().await;
    let cipher = CredentialCipher::from_base64(&CredentialCipher::generate_key_base64())
        .expect("密钥应可用");
    let cipher = Arc::new(cipher);

    // 注册表：内置凭据类型 + 内存 DNS 凭据类型。
    let mut credential_registry = CredentialRegistry::new();
    credential_registry
        .register(acmecast_access::AcmeAccountType::new())
        .expect("ACME 账号类型不应重复");
    credential_registry
        .register(MemoryDnsType)
        .expect("内存 DNS 类型不应重复");
    let credential_registry = Arc::new(credential_registry);
    // 写入与读取必须用同一把密钥：解密失败「密文校验不通过」多半是这里分叉了。
    let credentials =
        CredentialStore::new(&db, Arc::clone(&credential_registry), Arc::clone(&cipher));

    let account_id = an_acme_account_credential(&db, &cipher, &pebble.directory_url).await;

    // 内存 DNS 提供商进注册表，挑战记录的写入与清理都能被断言。
    let memory_dns = MemoryDns::default();
    let mut dns_registry = DnsProviderRegistry::new();
    dns_registry
        .register(memory_dns.clone())
        .expect("内存 DNS 提供商不应重复");
    let dns_registry = Arc::new(dns_registry);

    let data_dir = TempDir::new("e2e-data");
    let deploy_dir = TempDir::new("e2e-deploy");

    let mut deploy_registry = DeploymentRegistry::new();
    deploy_registry
        .register(acmecast_deploy::LocalTarget)
        .expect("本地目标不应重复");
    let deploy_registry = Arc::new(deploy_registry);

    // 步骤注册表：apply 用自定义 DNS 注册表；store/deploy 用测试的库与目录。
    let mut steps = StepRegistry::new();
    steps
        .register(CertApplyStep::new(Arc::clone(&dns_registry)))
        .expect("cert.apply 不应重复");
    steps
        .register(CertStoreStep::new(
            db.clone(),
            data_dir.path().to_path_buf(),
        ))
        .expect("cert.store 不应重复");
    steps
        .register(CertDeployStep::new(
            Arc::clone(&deploy_registry),
            Arc::new(InMemoryDeploymentState::new()),
        ))
        .expect("cert.deploy 不应重复");
    let steps = Arc::new(steps);

    let definition = PipelineDefinition {
        id: 1,
        steps: vec![
            acmecast_pipeline::StepDefinition {
                order_index: 0,
                type_id: "cert.apply".to_owned(),
                input: serde_json::json!({
                    "domains": ["e2e.acmecast.test"],
                    "challenge": "dns-01",
                    "account_credential_id": account_id,
                    "dns_provider": "memory",
                    "dns_credential_id": 1,
                    "wait_propagation": false,
                    "insecure_skip_verify": true
                }),
                enabled: true,
            },
            acmecast_pipeline::StepDefinition {
                order_index: 1,
                type_id: "cert.store".to_owned(),
                input: serde_json::json!({ "acme_account_credential_id": account_id }),
                enabled: true,
            },
            acmecast_pipeline::StepDefinition {
                order_index: 2,
                type_id: "cert.deploy".to_owned(),
                input: serde_json::json!({
                    "target": "local",
                    "config": {
                        "cert_path": deploy_dir.path().join("cert.pem").display().to_string(),
                        "key_path": deploy_dir.path().join("key.pem").display().to_string(),
                        "cert_mode": "0644",
                        "key_mode": "0600"
                    },
                    "force": false
                }),
                enabled: true,
            },
        ],
    };

    // dns_credential_id=1 需要一份内存 DNS 凭据记录。
    let dns_fields = serde_json::json!({});
    let encrypted = cipher.encrypt_string(&dns_fields.to_string()).unwrap();
    acmecast_store::entity::credential::ActiveModel {
        name: Set("内存 DNS".to_owned()),
        type_id: Set("dns.memory".to_owned()),
        encrypted_fields: Set(encrypted),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("DNS 凭据应能写入");

    let leaked: &'static DatabaseConnection = Box::leak(Box::new(db.clone()));
    let state = acmecast_pipeline::DatabaseStateStore::new(leaked);
    let runner = PipelineRunner::new(&steps, &credentials, &state);
    let outcome = runner.run(&definition, 1).await.expect("流水线应能执行");

    assert!(
        outcome.is_success(),
        "端到端流水线应成功，失败原因：{:?}",
        outcome.failure
    );

    // 1. 部署目录里有两份文件，内容是签发的证书与私钥。
    let cert_pem =
        std::fs::read_to_string(deploy_dir.path().join("cert.pem")).expect("证书应已部署");
    let key_pem = std::fs::read_to_string(deploy_dir.path().join("key.pem")).expect("私钥应已部署");
    assert!(cert_pem.contains("BEGIN CERTIFICATE"), "应是 PEM 证书");
    assert!(key_pem.contains("PRIVATE KEY"), "应是 PEM 私钥");

    // 2. 证书已入库：域名可查、账号凭据已绑定、有效期来自 pebble 签发。
    let repository = CertRepository::new(&db);
    let records = repository
        .list(CertQuery {
            domain: Some("acmecast.test".to_owned()),
            ..Default::default()
        })
        .await
        .expect("应能查询证书");
    assert_eq!(records.items.len(), 1, "入库应恰好一条记录");
    let stored = &records.items[0];
    assert_eq!(stored.acme_account_access_id, Some(account_id));
    let leaf = acmecast_cert::parse_pem_leaf(&cert_pem).expect("应能解析签发的证书");
    assert_eq!(stored.fingerprint, leaf.fingerprint_sha256);

    // 3. 账号凭据已写回：再次运行会复用而不是注册新账号。
    let account = acmecast_store::entity::credential::Entity::find_by_id(account_id)
        .one(&db)
        .await
        .expect("应能读取账号凭据")
        .expect("账号凭据应存在");
    let fields: serde_json::Value =
        serde_json::from_str(&cipher.decrypt_string(&account.encrypted_fields).unwrap())
            .expect("凭据字段应是 JSON");
    assert!(
        fields["credentials"].is_string(),
        "首次运行后账号凭据应写回记录：{fields}"
    );

    // 4. DNS-01 挑战记录已被清理（写 → 验证 → 删 的闭环）。
    let snapshot = memory_dns.snapshot();
    let record_name = challenge_record_name("e2e.acmecast.test");
    let remaining = snapshot
        .get(&record_name)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|value| !value.is_empty())
        .count();
    assert_eq!(remaining, 0, "挑战 TXT 记录应在完成后清理：{snapshot:?}");
}

/// zone 推导失败必须早于任何 DNS 调用：注册域本身（`skiy.net`）剥第一段
/// 只剩 TLD，历史上一度被推导成 `net` 拿去查 Cloudflare 才报错
/// （histories/26）。现在要早失败并提示显式配置，错误里带上域名。
#[tokio::test(flavor = "multi_thread")]
async fn zone_derivation_failure_fails_before_any_dns_call() {
    let pebble = match start_pebble() {
        Ok(pebble) => pebble,
        Err(reason) => {
            eprintln!("跳过 zone 推导失败测试：{reason}");
            return;
        }
    };

    let db = a_database().await;
    let cipher = Arc::new(
        CredentialCipher::from_base64(&CredentialCipher::generate_key_base64())
            .expect("密钥应可用"),
    );
    let mut credential_registry = CredentialRegistry::new();
    credential_registry
        .register(acmecast_access::AcmeAccountType::new())
        .expect("ACME 账号类型不应重复");
    credential_registry
        .register(MemoryDnsType)
        .expect("内存 DNS 类型不应重复");
    let credentials = CredentialStore::new(
        &db,
        Arc::new(credential_registry),
        Arc::clone(&cipher),
    );

    let account_id = an_acme_account_credential(&db, &cipher, &pebble.directory_url).await;
    let dns_credential_id = a_memory_dns_credential(&db, &cipher).await;

    let memory_dns = MemoryDns::default();
    let mut dns_registry = DnsProviderRegistry::new();
    dns_registry
        .register(memory_dns.clone())
        .expect("内存 DNS 提供商不应重复");
    let dns_registry = Arc::new(dns_registry);

    let mut steps = StepRegistry::new();
    steps
        .register(CertApplyStep::new(Arc::clone(&dns_registry)))
        .expect("cert.apply 不应重复");

    // 注册域本身不给 dns_zone：推导不出 zone，应在任何 DNS 调用前失败。
    let definition = PipelineDefinition {
        id: 1,
        steps: vec![acmecast_pipeline::StepDefinition {
            order_index: 0,
            type_id: "cert.apply".to_owned(),
            input: serde_json::json!({
                "domains": ["skiy.net"],
                "challenge": "dns-01",
                "account_credential_id": account_id,
                "dns_provider": "memory",
                "dns_credential_id": dns_credential_id,
                "wait_propagation": false,
                "insecure_skip_verify": true
            }),
            enabled: true,
        }],
    };

    let leaked: &'static DatabaseConnection = Box::leak(Box::new(db.clone()));
    let state = acmecast_pipeline::DatabaseStateStore::new(leaked);
    let runner = PipelineRunner::new(&steps, &credentials, &state);
    let outcome = runner.run(&definition, 1).await.expect("流水线应能执行");

    assert!(
        !outcome.is_success(),
        "无法推导 zone 的申请应失败：{:?}",
        outcome.failure
    );
    let failure = outcome.failure.as_ref().expect("应有失败详情");
    assert!(
        failure.reason.contains("skiy.net"),
        "失败原因应包含授权域名：{}",
        failure.reason
    );
    assert!(
        failure.reason.contains("请显式配置 dns_zone"),
        "失败原因应提示显式配置：{}",
        failure.reason
    );
    assert!(
        memory_dns.snapshot().is_empty(),
        "zone 推导失败不得触发任何 DNS 调用"
    );
}

/// 密钥类型可选：`key_algorithm = "ecdsa_p384"` 时，pebble 按同一算法签发，
/// 私钥识别为 ECDSA P-384，且与证书通过入库前的匹配校验（`detect_algorithm`
/// 与 `verify_matches_pem` 与 cert.store 入库前校验同源）。
#[tokio::test(flavor = "multi_thread")]
async fn applies_with_ecdsa_p384_key_algorithm() {
    let pebble = match start_pebble() {
        Ok(pebble) => pebble,
        Err(reason) => {
            eprintln!("跳过 P-384 密钥算法测试：{reason}");
            return;
        }
    };

    let db = a_database().await;
    let cipher = Arc::new(
        CredentialCipher::from_base64(&CredentialCipher::generate_key_base64())
            .expect("密钥应可用"),
    );

    let mut credential_registry = CredentialRegistry::new();
    credential_registry
        .register(acmecast_access::AcmeAccountType::new())
        .expect("ACME 账号类型不应重复");
    credential_registry
        .register(MemoryDnsType)
        .expect("内存 DNS 类型不应重复");
    let credential_registry = Arc::new(credential_registry);
    let credentials =
        CredentialStore::new(&db, Arc::clone(&credential_registry), Arc::clone(&cipher));

    let account_id = an_acme_account_credential(&db, &cipher, &pebble.directory_url).await;
    let dns_credential_id = a_memory_dns_credential(&db, &cipher).await;

    let memory_dns = MemoryDns::default();
    let mut dns_registry = DnsProviderRegistry::new();
    dns_registry
        .register(memory_dns.clone())
        .expect("内存 DNS 提供商不应重复");
    let dns_registry = Arc::new(dns_registry);

    let mut steps = StepRegistry::new();
    steps
        .register(CertApplyStep::new(Arc::clone(&dns_registry)))
        .expect("cert.apply 不应重复");

    let definition = PipelineDefinition {
        id: 1,
        steps: vec![acmecast_pipeline::StepDefinition {
            order_index: 0,
            type_id: "cert.apply".to_owned(),
            input: serde_json::json!({
                "domains": ["p384.acmecast.test"],
                "challenge": "dns-01",
                "key_algorithm": "ecdsa_p384",
                "account_credential_id": account_id,
                "dns_provider": "memory",
                "dns_credential_id": dns_credential_id,
                "wait_propagation": false,
                "insecure_skip_verify": true
            }),
            enabled: true,
        }],
    };

    let leaked: &'static DatabaseConnection = Box::leak(Box::new(db.clone()));
    let state = acmecast_pipeline::DatabaseStateStore::new(leaked);
    let runner = PipelineRunner::new(&steps, &credentials, &state);
    let outcome = runner.run(&definition, 1).await.expect("流水线应能执行");
    assert!(
        outcome.is_success(),
        "P-384 申请应成功，失败原因：{:?}",
        outcome.failure
    );

    let key_pem: String = serde_json::from_value(outcome.artifacts.get("key_pem").cloned().expect("应有 key_pem 产物"))
        .expect("key_pem 应是字符串");
    let cert_pem: String =
        serde_json::from_value(outcome.artifacts.get("cert_pem").cloned().expect("应有 cert_pem 产物"))
            .expect("cert_pem 应是字符串");
    assert_eq!(
        acmecast_cert::detect_algorithm(&key_pem).expect("识别私钥算法"),
        acmecast_cert::KeyAlgorithm::EcdsaP384,
        "签发的私钥应是 ECDSA P-384"
    );
    acmecast_cert::verify_matches_pem(&key_pem, &cert_pem).expect("私钥与签发证书应匹配");
}

// ---- 11.2 定时续期闭环 ----
//
// 调度引擎扫描命中证书（到期阈值被调大）→ 经真实启动器重签 →
// 去重窗口内第二次扫描被抑制。整条链路用与生产一致的组件。

#[tokio::test(flavor = "multi_thread")]
async fn renewal_scan_triggers_a_single_reissue() {
    let pebble = match start_pebble() {
        Ok(pebble) => pebble,
        Err(reason) => {
            eprintln!("跳过续期闭环测试：{reason}");
            return;
        }
    };

    let db = a_database().await;
    let cipher =
        Arc::new(CredentialCipher::from_base64(&CredentialCipher::generate_key_base64()).unwrap());
    let mut credential_registry = CredentialRegistry::new();
    credential_registry
        .register(acmecast_access::AcmeAccountType::new())
        .expect("ACME 账号类型不应重复");
    credential_registry
        .register(MemoryDnsType)
        .expect("内存 DNS 类型不应重复");
    let credential_registry = Arc::new(credential_registry);

    let account_id = an_acme_account_credential(&db, &cipher, &pebble.directory_url).await;
    let dns_credential_id = a_memory_dns_credential(&db, &cipher).await;

    let memory_dns = MemoryDns::default();
    let mut dns_registry = DnsProviderRegistry::new();
    dns_registry
        .register(memory_dns.clone())
        .expect("内存 DNS 提供商不应重复");
    let dns_registry = Arc::new(dns_registry);

    let data_dir = TempDir::new("renew-data");
    let deploy_dir = TempDir::new("renew-deploy");

    let mut deploy_registry = DeploymentRegistry::new();
    deploy_registry
        .register(acmecast_deploy::LocalTarget)
        .expect("本地目标不应重复");
    let deploy_registry = Arc::new(deploy_registry);

    let mut steps = StepRegistry::new();
    steps
        .register(CertApplyStep::new(Arc::clone(&dns_registry)))
        .expect("cert.apply 不应重复");
    steps
        .register(CertStoreStep::new(
            db.clone(),
            data_dir.path().to_path_buf(),
        ))
        .expect("cert.store 不应重复");
    steps
        .register(CertDeployStep::new(
            Arc::clone(&deploy_registry),
            Arc::new(InMemoryDeploymentState::new()),
        ))
        .expect("cert.deploy 不应重复");
    let steps = Arc::new(steps);

    // 流水线定义落库：调度启动器从库里读定义执行，与生产路径一致。
    let pipeline_id = acmecast_store::repository::PipelineRepository::new(&db)
        .save(
            None,
            acmecast_store::repository::PipelineInput {
                name: "续期 e2e.acmecast.test".to_owned(),
                description: None,
                enabled: true,
                steps: vec![
                    acmecast_store::repository::PipelineStepInput {
                        type_id: "cert.apply".to_owned(),
                        input: serde_json::json!({
                            "domains": ["e2e.acmecast.test"],
                            "challenge": "dns-01",
                            "account_credential_id": account_id,
                            "dns_provider": "memory",
                            "dns_credential_id": dns_credential_id,
                            "wait_propagation": false,
                            "insecure_skip_verify": true
                        }),
                        enabled: true,
                    },
                    acmecast_store::repository::PipelineStepInput {
                        type_id: "cert.store".to_owned(),
                        input: serde_json::json!({ "acme_account_credential_id": account_id }),
                        enabled: true,
                    },
                    acmecast_store::repository::PipelineStepInput {
                        type_id: "cert.deploy".to_owned(),
                        input: serde_json::json!({
                            "target": "local",
                            "config": {
                                "cert_path": deploy_dir.path().join("cert.pem").display().to_string(),
                                "key_path": deploy_dir.path().join("key.pem").display().to_string()
                            },
                            "force": false
                        }),
                        enabled: true,
                    },
                ],
            },
        )
        .await
        .expect("流水线应能保存");

    // 首次签发：手动触发一次，建立「当前已部署的证书」。
    let launcher = acmecast_server::scheduler::PipelineRunLauncher::new(
        Arc::clone(&steps),
        &db,
        Arc::clone(&credential_registry),
        Some(&cipher),
        None,
    );
    launcher
        .launch(acmecast_scheduler::LaunchRequest {
            pipeline_id,
            source: acmecast_store::entity::pipeline::TriggerSource::Manual,
            detail: None,
        })
        .await
        .expect("首次签发应成功");

    let repository = CertRepository::new(&db);
    let first = repository
        .list(CertQuery {
            domain: Some("e2e.acmecast.test".to_owned()),
            ..Default::default()
        })
        .await
        .expect("应能查询证书");
    assert_eq!(first.items.len(), 1);
    let first_fingerprint = first.items[0].fingerprint.clone();

    // 调度配置：只参与续期扫描，域名覆盖测试证书。
    acmecast_store::repository::ScheduleRepository::new(&db)
        .save(
            pipeline_id,
            acmecast_store::repository::ScheduleInput {
                cron: None,
                enabled: true,
                catch_up: false,
                renewal_domains: Some(vec!["e2e.acmecast.test".to_owned()]),
            },
        )
        .await
        .expect("调度应能保存");

    // 阈值调大到 90 天：pebble 刚签的证书（约 30 天有效期）立即进入续期窗口。
    let engine = acmecast_scheduler::SchedulerEngine::new(
        &db,
        &launcher,
        acmecast_scheduler::SchedulerConfig {
            renewal_threshold: chrono::Duration::days(90),
            ..Default::default()
        },
    );
    let now = chrono::Utc::now();

    engine.scan_expiring(now).await.expect("第一次扫描应成功");
    let deployed_now = std::fs::read_to_string(deploy_dir.path().join("cert.pem"))
        .expect("重签后证书应已重新部署");
    assert_ne!(
        acmecast_cert::parse_pem_leaf(&deployed_now)
            .expect("应能解析重签证书")
            .fingerprint_sha256,
        first_fingerprint,
        "重签后的指纹应不同（pebble 每次签发都是新证书）"
    );

    // 证书记录仍是同一条：按域名集合去重更新，而不是新增。
    let records = repository
        .list(CertQuery {
            domain: Some("e2e.acmecast.test".to_owned()),
            ..Default::default()
        })
        .await
        .expect("应能查询证书");
    assert_eq!(records.items.len(), 1, "重签应更新原记录而非新增");

    // 触发记录恰好一条续期触发。
    let logs = acmecast_store::repository::TriggerLogRepository::new(&db)
        .list(Some(pipeline_id), 1, 20)
        .await
        .expect("应能查询触发记录");
    assert_eq!(logs.total, 1, "应恰好一次续期触发：{:?}", logs.items);
    assert_eq!(
        logs.items[0].source,
        acmecast_store::entity::pipeline::TriggerSource::Renewal
    );

    // 去重窗口内再扫描：不重复触发。
    engine
        .scan_expiring(now + chrono::Duration::minutes(5))
        .await
        .expect("第二次扫描应成功");
    let logs = acmecast_store::repository::TriggerLogRepository::new(&db)
        .list(Some(pipeline_id), 1, 20)
        .await
        .expect("应能查询触发记录");
    assert_eq!(logs.total, 1, "去重窗口内的重复扫描不应再触发");
}

/// 预置一份内存 DNS 提供商凭据，返回主键。
async fn a_memory_dns_credential(db: &DatabaseConnection, cipher: &CredentialCipher) -> i64 {
    let encrypted = cipher
        .encrypt_string(&serde_json::json!({}).to_string())
        .expect("应能加密凭据");
    acmecast_store::entity::credential::ActiveModel {
        name: Set("内存 DNS".to_owned()),
        type_id: Set("dns.memory".to_owned()),
        encrypted_fields: Set(encrypted),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("DNS 凭据应能写入")
    .id
}

// ---- 11.3 吊销闭环 ----
//
// pebble 首签入库 → 经 HTTP 端点吊销 → CA 侧接受、本地状态更新；
// 再次吊销幂等返回；已吊销证书不再被续期扫描命中。

#[tokio::test(flavor = "multi_thread")]
async fn revocation_updates_ca_and_local_state() {
    let pebble = match start_pebble() {
        Ok(pebble) => pebble,
        Err(reason) => {
            eprintln!("跳过吊销闭环测试：{reason}");
            return;
        }
    };

    let db = a_database().await;
    let cipher =
        Arc::new(CredentialCipher::from_base64(&CredentialCipher::generate_key_base64()).unwrap());
    let mut credential_registry = CredentialRegistry::new();
    credential_registry
        .register(acmecast_access::AcmeAccountType::new())
        .expect("ACME 账号类型不应重复");
    credential_registry
        .register(MemoryDnsType)
        .expect("内存 DNS 类型不应重复");
    let credential_registry = Arc::new(credential_registry);
    let dns_credential_id = a_memory_dns_credential(&db, &cipher).await;

    let mut dns_registry = DnsProviderRegistry::new();
    dns_registry
        .register(MemoryDns::default())
        .expect("内存 DNS 提供商不应重复");
    let dns_registry = Arc::new(dns_registry);

    let data_dir = TempDir::new("revoke-data");
    let mut deploy_registry = DeploymentRegistry::new();
    deploy_registry
        .register(acmecast_deploy::LocalTarget)
        .expect("本地目标不应重复");
    let deploy_registry = Arc::new(deploy_registry);

    let mut steps = StepRegistry::new();
    steps
        .register(CertApplyStep::new(Arc::clone(&dns_registry)))
        .expect("cert.apply 不应重复");
    steps
        .register(CertStoreStep::new(
            db.clone(),
            data_dir.path().to_path_buf(),
        ))
        .expect("cert.store 不应重复");
    steps
        .register(CertDeployStep::new(
            deploy_registry,
            Arc::new(InMemoryDeploymentState::new()),
        ))
        .expect("cert.deploy 不应重复");
    let steps = Arc::new(steps);

    // 账号凭据 + 流水线落库，经启动器首签（吊销前提：账号已在 CA 侧建立）。
    let account_id = an_acme_account_credential(&db, &cipher, &pebble.directory_url).await;
    let pipeline_id = acmecast_store::repository::PipelineRepository::new(&db)
        .save(
            None,
            acmecast_store::repository::PipelineInput {
                name: "吊销 e2e.acmecast.test".to_owned(),
                description: None,
                enabled: true,
                steps: vec![
                    acmecast_store::repository::PipelineStepInput {
                        type_id: "cert.apply".to_owned(),
                        input: serde_json::json!({
                            "domains": ["revoke.acmecast.test"],
                            "challenge": "dns-01",
                            "account_credential_id": account_id,
                            "dns_provider": "memory",
                            "dns_credential_id": dns_credential_id,
                            "wait_propagation": false,
                            "insecure_skip_verify": true,
                            "contacts": ["ops@acmecast.test"]
                        }),
                        enabled: true,
                    },
                    acmecast_store::repository::PipelineStepInput {
                        type_id: "cert.store".to_owned(),
                        input: serde_json::json!({ "acme_account_credential_id": account_id }),
                        enabled: true,
                    },
                ],
            },
        )
        .await
        .expect("流水线应能保存");

    let launcher = acmecast_server::scheduler::PipelineRunLauncher::new(
        Arc::clone(&steps),
        &db,
        Arc::clone(&credential_registry),
        Some(&cipher),
        None,
    );
    launcher
        .launch(acmecast_scheduler::LaunchRequest {
            pipeline_id,
            source: acmecast_store::entity::pipeline::TriggerSource::Manual,
            detail: None,
        })
        .await
        .expect("首次签发应成功");

    // 账号注册用了 contacts：执行时已把裸邮箱补全为 mailto: 发给 CA（否则
    // pebble 会拒收），而库里保存的仍是用户输入的裸邮箱原样。
    let apply_step = acmecast_store::entity::pipeline_step::Entity::find()
        .filter(acmecast_store::entity::pipeline_step::Column::PipelineId.eq(pipeline_id))
        .filter(acmecast_store::entity::pipeline_step::Column::TypeId.eq("cert.apply"))
        .one(&db)
        .await
        .expect("应能查询流水线步骤")
        .expect("cert.apply 步骤应存在");
    assert_eq!(
        apply_step.input.get("contacts"),
        Some(&serde_json::json!(["ops@acmecast.test"])),
        "contacts 应按用户输入原样入库：{:?}",
        apply_step.input.get("contacts")
    );

    // HTTP 层复用同一个库：吊销端点读到的就是首签入库的记录。
    // 注册表带上 cert.apply，任务 schema 端点才有它的输入定义可查。
    let mut router_steps = StepRegistry::new();
    router_steps
        .register(CertApplyStep::new(Arc::clone(&dns_registry)))
        .expect("cert.apply 不应重复");
    let app = router_with_db(
        db.clone(),
        acmecast_server::config::ServerConfig {
            data_dir: data_dir.path().to_path_buf(),
            accept_invalid_acme_certs: true,
            ..Default::default()
        },
        acmecast_server::auth::AuthMode::Disabled,
        Some(Arc::clone(&cipher)),
        Arc::clone(&credential_registry),
        Arc::new(router_steps),
    )
    .await;

    let certificate = acmecast_store::repository::CertRepository::new(&db)
        .list(CertQuery {
            domain: Some("revoke.acmecast.test".to_owned()),
            ..Default::default()
        })
        .await
        .expect("应能查询证书");
    let cert_id = certificate.items[0].id;

    // cert.apply 的输入 schema 应向用户放宽 contacts 描述（前端 tooltip 的来源）。
    let schema = send(
        &app,
        json_request(
            "GET",
            "/api/tasks/cert.apply/schema",
            &serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(schema.status(), StatusCode::OK, "schema 端点应可查");
    let schema_body = json(schema).await;
    let contacts_description = schema_body["data"]["properties"]["contacts"]["description"]
        .as_str()
        .expect("contacts 应有描述");
    assert!(
        contacts_description.contains("mailto:") && contacts_description.contains("自动补全"),
        "contacts 描述应说明前缀可省略并由服务端补全：{contacts_description}"
    );

    // dns_zone 应携带「多域名必填」条件扩展：前端 SchemaForm 据此在
    // domains ≥ 2 时联动必填（多 zone 语义要求用户显式配置）。
    let dns_zone = &schema_body["data"]["properties"]["dns_zone"];
    assert_eq!(
        dns_zone["x-required-when"],
        serde_json::json!({ "MinItems": { "field": "domains", "count": 2 } }),
        "dns_zone 应携带多域名必填条件：{dns_zone}"
    );
    assert!(
        dns_zone["description"]
            .as_str()
            .unwrap_or_default()
            .contains("多个域名"),
        "dns_zone 描述应包含多域名指引：{}",
        dns_zone["description"]
    );

    // 吊销：CA 侧接受，本地状态更新。
    let response = send(
        &app,
        json_request(
            "POST",
            &format!("/api/certificates/{cert_id}/revoke"),
            &serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "吊销应成功");
    let body = json(response).await;
    assert_eq!(body["data"]["revoked_now"], true, "本次应完成吊销：{body}");

    let stored = acmecast_store::entity::cert::Entity::find_by_id(cert_id)
        .one(&db)
        .await
        .expect("应能读取证书")
        .expect("证书应存在");
    assert!(stored.revoked_at.is_some(), "本地吊销状态应已更新");

    // 吊销归档：材料移进吊销目录，库中路径同步更新，下载仍可用。
    // 归档只换目录不改名：签发时的域名文件名原样进入吊销目录。
    assert_eq!(
        stored.cert_pem_path,
        format!(
            "certs/revoked/{}/revoke.acmecast.test.cert.pem",
            stored.fingerprint
        ),
        "库中证书链路径应指向吊销目录，实际 {}",
        stored.cert_pem_path
    );
    assert_eq!(
        stored.key_pem_path,
        format!(
            "certs/revoked/{}/revoke.acmecast.test.key.pem",
            stored.fingerprint
        ),
        "库中私钥路径应指向吊销目录"
    );
    assert!(
        !data_dir
            .path()
            .join(format!(
                "certs/{}/revoke.acmecast.test.cert.pem",
                stored.fingerprint
            ))
            .exists(),
        "原位置的证书链不应保留"
    );
    assert!(
        !data_dir
            .path()
            .join(format!("certs/{}", stored.fingerprint))
            .exists(),
        "搬空的源目录不应留下空壳"
    );
    assert!(
        data_dir.path().join(&stored.cert_pem_path).is_file(),
        "吊销目录里应有证书链"
    );
    assert!(
        data_dir.path().join(&stored.key_pem_path).is_file(),
        "吊销目录里应有私钥"
    );

    let download = send(
        &app,
        axum::http::Request::builder()
            .method("GET")
            .uri(format!("/api/certificates/{cert_id}/download"))
            .body(axum::body::Body::empty())
            .expect("应能构造下载请求"),
    )
    .await;
    assert_eq!(download.status(), StatusCode::OK, "吊销后下载仍应可用");
    let bytes = axum::body::to_bytes(download.into_body(), 1024 * 1024)
        .await
        .expect("应能读取下载内容");
    assert!(
        String::from_utf8_lossy(&bytes).contains("BEGIN CERTIFICATE"),
        "吊销后下载的应是证书链"
    );

    // 已吊销的证书不再进入续期窗口。
    let due = CertRepository::new(&db)
        .list_due(chrono::Utc::now() + chrono::Duration::days(90))
        .await
        .expect("应能扫描");
    assert!(
        due.iter().all(|cert| cert.id != cert_id),
        "已吊销的证书不应被续期扫描命中"
    );

    // 再次吊销：本地记录已是终态，幂等返回且不再调用 CA。
    let response = send(
        &app,
        json_request(
            "POST",
            &format!("/api/certificates/{cert_id}/revoke"),
            &serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["data"]["revoked_now"], false, "重复吊销应幂等：{body}");

    // 无账号标识的记录（手动上传）：明确报错而非静默跳过。
    acmecast_store::repository::CertRepository::new(&db)
        .save(CertInput {
            domains: vec!["manual.acmecast.test".to_owned()],
            cert_pem_path: "certs/manual.pem".to_owned(),
            key_pem_path: "keys/manual.pem".to_owned(),
            fingerprint: "sha256:manual".to_owned(),
            issuer: None,
            not_before: chrono::Utc::now() - chrono::Duration::days(1),
            not_after: chrono::Utc::now() + chrono::Duration::days(90),
            acme_account_access_id: None,
        })
        .await
        .expect("手动上传证书应能入库");
    let manual = acmecast_store::repository::CertRepository::new(&db)
        .list(CertQuery {
            domain: Some("manual.acmecast.test".to_owned()),
            ..Default::default()
        })
        .await
        .expect("应能查询证书");
    let manual_id = manual.items[0].id;
    let response = send(
        &app,
        json_request(
            "POST",
            &format!("/api/certificates/{manual_id}/revoke"),
            &serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = json(response).await;
    assert_eq!(body["error"]["code"], "missing_account");
}
