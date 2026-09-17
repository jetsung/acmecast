{
  description = "acmecast（ACME 证书签发与部署服务）开发环境";

  inputs = {
    # nixos-unstable 分支（Hydra 测过的 channel），具体 commit 由 flake.lock 钉住：
    # 各机器复现靠 lock，升级用 `nix flake update nixpkgs`（改 URL 里的 sha 反而是死路，
    # rev 形式的输入 update 不会前进）。
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # Rust 工具链按版本提供（nixpkgs 只有最新 stable），与 Dockerfile 的 rust:1.98 对齐。
    rust-overlay = {
      url = "github:oxalica/rust-overlay/a1d497a9415999853809ba22a7959b1c36dc6554";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, rust-overlay, ... }:
    let
      # 覆盖 Linux 与 Apple Silicon macOS。
      # 特意不含 x86_64-darwin：钉住的 nixpkgs 26.11 已放弃 Intel Mac 支持（求值直接 throw），
      # Intel Mac 需把 nixpkgs 换回仍在维护的 26.05 分支。
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems f;
    in
    {
      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };
          # 版本与 rustfmt.toml 的 msrv = "1.98"、Dockerfile 的 rust:1.98 一致。
          rust = pkgs.rust-bin.stable."1.98.0".default.override {
            extensions = [
              "clippy"
              "rustfmt"
              "rust-analyzer"
            ];
          };
          # crates.io 镜像：cargo 的源替换不认环境变量（实测 CARGO_SOURCE_* 被忽略，
          # 索引仍落回 index.crates.io），只能用 --config 或配置文件。这里不落配置文件，
          # 改为提供一个 cargo 包装：只有 IS_CHINA=1 且子命令会访问 registry 时才追加
          # --config，其余调用（--version、fmt、new、clean…）原样直通。
          # 排在 PATH 最前，因此 `nix develop -c cargo …` 与 prek 钩子同样覆盖。
          cargoWrapped = pkgs.writeShellScriptBin "cargo" ''
            real=${rust}/bin/cargo
            if [ "''${IS_CHINA:-0}" != 1 ]; then
              exec "$real" "$@"
            fi

            # 取子命令：跳过全局选项（-x / --x）与 +toolchain
            sub=
            for arg in "$@"; do
              case "$arg" in
                --) break ;;
                -*) continue ;;
                +*) continue ;;
                *)
                  sub="$arg"
                  break
                  ;;
              esac
            done

            case "$sub" in
              fetch | build | check | clippy | test | bench | run | doc | rustdoc | rustc | package | publish | install | update | add | remove | tree | metadata | vendor | generate-lockfile | search)
                exec "$real" \
                  --config 'source.crates-io.replace-with="tencent"' \
                  --config 'source.tencent.registry="sparse+https://mirrors.cloud.tencent.com/cargo/"' \
                  "$@"
                ;;
              *)
                exec "$real" "$@"
                ;;
            esac
          '';
        in
        {
          default = pkgs.mkShell {
            packages =
              [
                rust
                # 前端：node 24 + pnpm 11（对应 package.json 的 packageManager）
                pkgs.nodejs_24
                pkgs.pnpm
                # Git 钩子（prek.toml）：提交前做格式/卫生检查，推送前跑 clippy
                pkgs.prek
                # cargo 包装（crates.io 镜像按 IS_CHINA 注入），需排在 rust 之前生效，见 shellHook
                cargoWrapped
                # 日常排查
                pkgs.jq
                pkgs.tree
                pkgs.curl
                pkgs.git
		pkgs.tree
		pkgs.tmux
                pkgs.cacert
                pkgs.sqlite
              ]
              # 这四样是 Linux 向的：darwin 上链接器和构建脚本（ring / aws-lc-sys）
              # 走 stdenv 自带的 clang，不需要也不该再塞一个 GNU 工具链进 PATH。
              ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
                pkgs.gcc
                pkgs.pkg-config
                pkgs.gnumake
                pkgs.perl
              ];

            shellHook = ''
              # cargo 包装要排在最前，才能盖住 rust 工具链自带的 cargo
              export PATH="${cargoWrapped}/bin:$PATH"
              # utoipa-swagger-ui 的 build.rs 默认从 GitHub 下载压缩包；
              # 指向仓库内随附副本，构建不依赖外网（同 Dockerfile）。
              export SWAGGER_UI_DOWNLOAD_URL="file://$PWD/docker/swagger-ui-v5.17.14.zip"
              echo "acmecast dev shell: $(rustc --version) / $(node --version) / pnpm $(pnpm --version)"
            '';
          };
        }
      );
    };
}
