//! 临时：打印 cert.apply 的真实 schemars 产物。
use acmecast_server::steps::apply::AcmeApplyInput;
use acmecast_server::steps::store::CertStoreInput;

#[test]
fn dump() {
    println!(
        "APPLY={}",
        serde_json::to_string(&schemars::schema_for!(AcmeApplyInput)).unwrap()
    );
    println!(
        "STORE={}",
        serde_json::to_string(&schemars::schema_for!(CertStoreInput)).unwrap()
    );
}
