//! 测试用的 mock ACME 服务器。
//!
//! 通过实现 [`instant_acme::HttpClient`] 拦截 ACME 请求，在进程内按路径分派预设响应。
//! 相比起一个真实的 HTTP 服务器，这能在不引入网络依赖的前提下精确控制
//! badNonce、订单状态迁移与调用次数，正是 spec 3.7 / 3.8 需要的可观测性。
//!
//! 本模块只在测试或显式启用 `testing` feature 时编译。

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http_body_util::BodyExt;
use instant_acme::{BodyWrapper, BytesBody, BytesResponse, Error as AcmeLibError, HttpClient, Key};

use crate::account::AccountCredentials;

/// mock 服务器的基地址。
pub const BASE: &str = "https://ca.test";

/// 造一份可直接使用的账号凭据。
///
/// 私钥由底层库自己生成，因此必然被它自己接受——用外部工具（openssl 等）生成的
/// PKCS#8 未必满足它对编码的严格要求，拿那种密钥拼出来的凭据会在恢复会话时才失败。
pub fn sample_credentials(kid: &str, directory_url: &str) -> AccountCredentials {
    let (_, pkcs8) = Key::generate_pkcs8().expect("应能生成密钥");
    AccountCredentials::from_parts(kid, &pkcs8, directory_url)
}

/// 一笔请求记录：HTTP 方法、完整 URL 与请求体（JWS 的 Flattened JSON 文本）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRequest {
    /// HTTP 方法。
    pub method: String,
    /// 完整 URL。
    pub url: String,
    /// 请求体原文。POST-AS-GET 为空串；其余是 JWS 包裹的 JSON。
    pub body: String,
}

impl RecordedRequest {
    /// URL 是否以给定路径结尾。
    #[must_use]
    pub fn ends_with(&self, path: &str) -> bool {
        self.url.ends_with(path)
    }
}

/// 测试用 ACME 服务器。
#[derive(Clone, Debug)]
pub struct MockAcme {
    state: Arc<Mutex<MockState>>,
}

#[derive(Debug)]
struct MockState {
    requests: Vec<RecordedRequest>,
    nonce_seq: u64,

    /// 还需要返回 badNonce 的 POST 次数。
    bad_nonce_remaining: usize,

    /// 授权状态：`pending` / `valid` / `invalid`。
    authz_status: String,
    /// 订单当前状态。
    order_status: String,
    /// finalize 之后订单转为该状态。
    order_status_after_finalize: String,
    /// `processing` 状态下经过多少次 refresh 后转为 `valid`。
    processing_refreshes: usize,
    /// refresh 被调用的次数。
    refresh_calls: usize,
    /// finalize 被调用的次数。
    finalize_calls: usize,

    /// 下载证书时返回的 PEM 内容。
    cert_pem: String,
}

impl Default for MockState {
    fn default() -> Self {
        Self {
            requests: Vec::new(),
            nonce_seq: 0,
            bad_nonce_remaining: 0,
            authz_status: "pending".to_owned(),
            order_status: "pending".to_owned(),
            order_status_after_finalize: "valid".to_owned(),
            processing_refreshes: 0,
            refresh_calls: 0,
            finalize_calls: 0,
            cert_pem: "-----BEGIN CERTIFICATE-----\nMOCK\n-----END CERTIFICATE-----\n".to_owned(),
        }
    }
}

impl Default for MockAcme {
    fn default() -> Self {
        Self::new()
    }
}

impl MockAcme {
    /// 新建一个默认行为的 mock：授权 pending、订单 pending、finalize 后直接 valid。
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(MockState::default())),
        }
    }

    /// 让接下来的 `times` 次 POST 返回 badNonce 错误。
    #[must_use]
    pub fn with_bad_nonce(self, times: usize) -> Self {
        self.state.lock().unwrap().bad_nonce_remaining = times;
        self
    }

    /// 设置授权状态。
    #[must_use]
    pub fn with_authz_status(self, status: &str) -> Self {
        self.state.lock().unwrap().authz_status = status.to_owned();
        self
    }

    /// 设置 finalize 之后订单转为的状态。
    #[must_use]
    pub fn with_order_status_after_finalize(self, status: &str) -> Self {
        self.state.lock().unwrap().order_status_after_finalize = status.to_owned();
        self
    }

    /// 让订单在 `processing` 状态下经历这么多次 refresh 后才转为 `valid`。
    #[must_use]
    pub fn with_processing_refreshes(self, count: usize) -> Self {
        self.state.lock().unwrap().processing_refreshes = count;
        self
    }

    /// 设置下载返回的证书内容。
    #[must_use]
    pub fn with_cert_pem(self, pem: &str) -> Self {
        self.state.lock().unwrap().cert_pem = pem.to_owned();
        self
    }

    /// 全部请求记录。
    #[must_use]
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.state.lock().unwrap().requests.clone()
    }

    /// 命中指定路径的请求次数。
    #[must_use]
    pub fn count_for(&self, path_suffix: &str) -> usize {
        self.requests()
            .iter()
            .filter(|r| r.ends_with(path_suffix))
            .count()
    }

    /// 最近一次命中指定路径的请求的 JWS payload 明文。
    ///
    /// ACME 的请求体是 Flattened JSON JWS，payload 为 base64url（无填充）编码的
    /// JSON——断言「实际发给 CA 的内容」时需要解开它。没有命中记录或解码失败
    /// 时返回 `None`，由调用方决定怎么报错。
    #[must_use]
    pub fn payload_for(&self, path_suffix: &str) -> Option<String> {
        let requests = self.requests();
        let record = requests.iter().rev().find(|r| r.ends_with(path_suffix))?;
        let jws: serde_json::Value = serde_json::from_str(&record.body).ok()?;
        let encoded = jws.get("payload")?.as_str()?;
        use base64::Engine as _;
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .ok()?;
        String::from_utf8(decoded).ok()
    }

    /// finalize 被调用的次数。
    #[must_use]
    pub fn finalize_calls(&self) -> usize {
        self.state.lock().unwrap().finalize_calls
    }

    /// 生成一个递增的 nonce。
    fn next_nonce(state: &mut MockState) -> String {
        state.nonce_seq += 1;
        format!("nonce-{}", state.nonce_seq)
    }
}

/// 构造一个响应。
fn build_response(
    status: u16,
    nonce: Option<&str>,
    content_type: &str,
    body: Vec<u8>,
) -> BytesResponse {
    let mut builder = http::Response::builder()
        .status(status)
        .header("content-type", content_type);
    if let Some(nonce) = nonce {
        builder = builder.header("Replay-Nonce", nonce);
    }
    let response = builder
        .body(Bytes::from(body))
        .expect("mock 响应构造不应失败");
    let (parts, body) = response.into_parts();
    BytesResponse {
        parts,
        body: Box::new(body) as Box<dyn BytesBody>,
    }
}

fn json_response(status: u16, nonce: &str, value: &serde_json::Value) -> BytesResponse {
    build_response(
        status,
        Some(nonce),
        "application/json",
        serde_json::to_vec(value).expect("JSON 序列化不应失败"),
    )
}

fn problem_response(status: u16, nonce: &str, kind: &str, detail: &str) -> BytesResponse {
    json_response(
        status,
        nonce,
        &serde_json::json!({
            "type": format!("urn:ietf:params:acme:error:{kind}"),
            "detail": detail,
            "status": status,
        }),
    )
}

impl HttpClient for MockAcme {
    fn request(
        &self,
        req: http::Request<BodyWrapper<Bytes>>,
    ) -> Pin<Box<dyn Future<Output = Result<BytesResponse, AcmeLibError>> + Send>> {
        let mock = self.clone();

        Box::pin(async move {
            let method = req.method().to_string();
            let url = req.uri().to_string();
            let body = req
                .into_body()
                .collect()
                .await
                .map(|collected| {
                    String::from_utf8_lossy(collected.to_bytes().as_ref()).into_owned()
                })
                .unwrap_or_default();

            let mut state = mock.state.lock().unwrap();
            state.requests.push(RecordedRequest {
                method: method.clone(),
                url: url.clone(),
                body,
            });
            let nonce = Self::next_nonce(&mut state);

            // 路径分派：去掉基地址后按尾部匹配。
            let path = url.strip_prefix(BASE).unwrap_or(&url).to_owned();

            // GET /directory
            if path == "/directory" {
                return Ok(json_response(
                    200,
                    &nonce,
                    &serde_json::json!({
                        "newNonce": format!("{BASE}/new-nonce"),
                        "newAccount": format!("{BASE}/new-account"),
                        "newOrder": format!("{BASE}/new-order"),
                        "revokeCert": format!("{BASE}/revoke-cert"),
                        "keyChange": format!("{BASE}/key-change"),
                    }),
                ));
            }

            // HEAD /new-nonce：spec 要求 200 + Replay-Nonce。
            if path == "/new-nonce" {
                return Ok(build_response(
                    200,
                    Some(&nonce),
                    "application/octet-stream",
                    vec![],
                ));
            }

            // 其余均为 POST。badNonce 注入只作用于它们。
            if state.bad_nonce_remaining > 0 {
                state.bad_nonce_remaining -= 1;
                return Ok(problem_response(
                    400,
                    &nonce,
                    "badNonce",
                    "JWS has an invalid anti-replay nonce",
                ));
            }

            // POST /new-account：201 + Location(KID)
            if path == "/new-account" {
                let mut rsp = json_response(201, &nonce, &serde_json::json!({"status": "valid"}));
                rsp.parts.headers.insert(
                    "Location",
                    format!("{BASE}/acct/1").parse().expect("标头值应合法"),
                );
                return Ok(rsp);
            }

            // POST /new-order：201 + Location(order URL)
            if path == "/new-order" {
                let mut rsp = json_response(201, &nonce, &order_state(&state));
                rsp.parts.headers.insert(
                    "Location",
                    format!("{BASE}/order/1").parse().expect("标头值应合法"),
                );
                return Ok(rsp);
            }

            // POST /authz/1：`example.com`，只提供 dns-01
            if path == "/authz/1" {
                return Ok(json_response(
                    200,
                    &nonce,
                    &authorization_state("example.com", &state.authz_status, "dns-01"),
                ));
            }

            // POST /authz/2：`wildcard.example.com`，只提供 http-01。
            // 与 authz/1 使用不同标识符，才能分别验证两种挑战的应答材料。
            if path == "/authz/2" {
                return Ok(json_response(
                    200,
                    &nonce,
                    &authorization_state("wildcard.example.com", &state.authz_status, "http-01"),
                ));
            }

            // POST /chall/1：挑战就绪
            if path == "/chall/1" {
                return Ok(json_response(
                    200,
                    &nonce,
                    &serde_json::json!({
                        "type": "dns-01",
                        "url": format!("{BASE}/chall/1"),
                        "token": "mock-token",
                        "status": "processing",
                    }),
                ));
            }

            // POST /order/1/finalize
            if path == "/order/1/finalize" {
                state.finalize_calls += 1;
                state.order_status = state.order_status_after_finalize.clone();
                return Ok(json_response(200, &nonce, &order_state(&state)));
            }

            // POST /order/1：刷新订单状态
            if path == "/order/1" {
                state.refresh_calls += 1;
                if state.order_status == "processing"
                    && state.refresh_calls > state.processing_refreshes
                {
                    state.order_status = "valid".to_owned();
                }
                return Ok(json_response(200, &nonce, &order_state(&state)));
            }

            // POST /cert/1：证书下载
            if path == "/cert/1" {
                return Ok(build_response(
                    200,
                    Some(&nonce),
                    "application/pem-certificate-chain",
                    state.cert_pem.clone().into_bytes(),
                ));
            }

            // POST /revoke-cert
            if path == "/revoke-cert" {
                return Ok(build_response(
                    200,
                    Some(&nonce),
                    "application/json",
                    vec![],
                ));
            }

            Ok(problem_response(
                404,
                &nonce,
                "malformed",
                &format!("mock 未实现的路径: {path}"),
            ))
        })
    }
}

/// 按当前状态构造订单 JSON。
fn order_state(state: &MockState) -> serde_json::Value {
    let mut value = serde_json::json!({
        "status": state.order_status,
        "identifiers": [
            {"type": "dns", "value": "example.com"},
            {"type": "dns", "value": "wildcard.example.com"},
        ],
        "authorizations": [
            format!("{BASE}/authz/1"),
            format!("{BASE}/authz/2"),
        ],
        "finalize": format!("{BASE}/order/1/finalize"),
    });

    // 只有 valid 状态才提供证书 URL——与真实 CA 行为一致。
    if state.order_status == "valid" {
        value["certificate"] = serde_json::json!(format!("{BASE}/cert/1"));
    }

    value
}

/// 构造授权 JSON。
fn authorization_state(identifier: &str, status: &str, challenge_type: &str) -> serde_json::Value {
    serde_json::json!({
        "identifier": {"type": "dns", "value": identifier},
        "status": status,
        "challenges": [
            {
                "type": challenge_type,
                "url": format!("{BASE}/chall/1"),
                "token": "mock-token",
                "status": if status == "valid" { "valid" } else { "pending" },
            }
        ],
        "wildcard": false,
    })
}
