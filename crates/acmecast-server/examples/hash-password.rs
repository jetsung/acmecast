//! 生成管理员口令的 Argon2 PHC 哈希，供 `ACMECAST_ADMIN_PASSWORD_HASH` 使用。
//!
//! 开发环境用法：`cargo run -p acmecast-server --example hash-password -- '你的口令'`
//! 运行环境（镜像内）等价命令：`docker run --rm acmecast:dev hash-password '你的口令'`

fn main() {
    let password = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("用法：cargo run -p acmecast-server --example hash-password -- '<口令>'");
        std::process::exit(1);
    });

    match acmecast_server::hash_password(&password) {
        Ok(hash) => println!("{hash}"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
