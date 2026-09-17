//! 生成管理员口令的 Argon2 PHC 哈希，供 `ACMECAST_ADMIN_PASSWORD_HASH` 使用。
//!
//! 用法：`cargo run -p acmecast-server --example hash-password -- '你的口令'`

use argon2::{Argon2, PasswordHasher};

fn main() {
    let password = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("用法：cargo run -p acmecast-server --example hash-password -- '<口令>'");
        std::process::exit(1);
    });
    if password.len() < 8 {
        eprintln!("口令至少 8 个字符");
        std::process::exit(1);
    }

    let hash = Argon2::default()
        .hash_password(password.as_bytes())
        .expect("应能生成口令哈希")
        .to_string();
    println!("{hash}");
}
