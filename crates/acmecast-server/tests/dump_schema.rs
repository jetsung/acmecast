use acmecast_server::openapi::ApiDoc;
use utoipa::OpenApi;

#[test]
fn dump() {
    let json = ApiDoc::openapi().to_pretty_json().unwrap();
    std::fs::write("/tmp/acmecast-openapi.json", json).unwrap();
}
