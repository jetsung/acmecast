//! 把 OpenAPI schema 导出到 `/tmp/acmecast-openapi.json`，供 `just gen-api` 使用。
use acmecast_server::openapi::ApiDoc;
use utoipa::OpenApi;

#[test]
fn dump() {
    let json = ApiDoc::openapi().to_pretty_json().unwrap();
    std::fs::write("/tmp/acmecast-openapi.json", json).unwrap();
}
