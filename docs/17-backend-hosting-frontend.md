# 17. 无 Docker 部署（nginx 反向代理合并前后端）

不使用 Docker 的**生产部署**教程：后端以源码构建的二进制常驻运行，前端产物由
nginx 托管，`/api` 反向代理到后端——对外只有一个端口，前后端同源、无 CORS。
密钥生成、口令哈希等准备工作与 Docker 方式完全一致，只是不再有容器。

> 只想在本机快速跑通（开发姿态、Vite dev server 代替 nginx）？
> 见 [15-local-development.md](15-local-development.md)。
> Docker 部署见 [13-docker-deployment.md](13-docker-deployment.md)。

## 17.1 架构

```
浏览器 ──> nginx :443/:80
             ├── /            ─> frontend/dist 静态文件（SPA，未命中回退 index.html）
             ├── /api/        ─> http://127.0.0.1:8080（acmecast-server）
             └── /swagger-ui  ─> http://127.0.0.1:8080（可选）
```

- nginx 只暴露 80/443；后端只监听 `127.0.0.1:8080`，不对公网开放；
- 后端自身也能托管前端（`ACMECAST_STATIC_DIR`，见
  [03-configuration.md](03-configuration.md) 3.4），用 nginx 的价值在于 TLS 终止、
  gzip、静态资源缓存与访问日志——这些由 nginx 做更专业。

## 17.2 前置条件

- 一台 Linux 服务器与 systemd（或其他进程管理器）；
- Rust 1.98+（构建后端，构建完可卸载）、Node 24 + pnpm（构建前端）；
  工具链可由仓库自带的 mise 配置提供，见
  [02-getting-started.md](02-getting-started.md) 2.2；
- nginx；
- 一个可用的 ACME CA 与 DNS 服务商账号（DNS-01 需要，见
  [07-dns-and-deploy.md](07-dns-and-deploy.md)）。

> 调试先用 `letsencrypt-staging` 端点，避免触发生产速率限制，见 17.7。

## 17.3 构建后端与前端

在源码目录（或 CI 机）执行：

```bash
# 后端：release 构建，产物在 target/release/acmecast-server
cargo build --release -p acmecast-server

# 前端：静态产物在 frontend/dist
cd frontend && pnpm install && pnpm build
```

部署到服务器（示例布局）：

```bash
sudo mkdir -p /opt/acmecast/{bin,frontend,data}
sudo install -m 0755 target/release/acmecast-server /opt/acmecast/bin/
sudo rsync -a --delete frontend/dist/ /opt/acmecast/frontend/
```

- 数据目录 `/opt/acmecast/data` 归运行服务的用户所有（下文用专用用户
  `acmecast`），**不要** `chmod 0777`（那是给容器 uid 65532 的做法）：

  ```bash
  sudo useradd --system --home /opt/acmecast --shell /usr/sbin/nologin acmecast
  sudo chown -R acmecast:acmecast /opt/acmecast/data
  ```

- `/opt/acmecast/frontend` 归 root 所有即可——nginx 只读它，后端不碰它。

## 17.4 生成密钥与口令哈希

与 Docker 方式一致（含义见 [13-docker-deployment.md](13-docker-deployment.md) 13.2/13.3）：

```bash
export ACMECAST_JWT_SECRET="$(openssl rand -base64 32)"
export ACMECAST_CREDENTIAL_KEY="$(openssl rand -base64 32)"
export ACMECAST_ADMIN_PASSWORD_HASH="$(cargo run -q -p acmecast-server --example hash-password -- '你的口令')"
```

> 没有现成二进制时，口令哈希用 `cargo run --example hash-password` 生成（如上）；
> 也可以先部署二进制，再用
> `/opt/acmecast/bin/acmecast-server hash-password '<口令>'` 生成。

三个变量请与数据目录分开保管（如写入 root 专属的 `/etc/acmecast/env`，
见 17.5）。**更换 `ACMECAST_CREDENTIAL_KEY` 会使已存凭据无法解密。**

## 17.5 常驻运行后端（systemd）

`/etc/acmecast/env`（root 属主、0600，`EnvironmentFile=` 读取）：

```ini
ACMECAST_LISTEN=127.0.0.1:8080
ACMECAST_DATA_DIR=/opt/acmecast/data
ACMECAST_JWT_SECRET=<17.4 生成的值>
ACMECAST_CREDENTIAL_KEY=<17.4 生成的值>
ACMECAST_ADMIN_PASSWORD_HASH=<17.4 生成的值>
# 前端由 nginx 托管时后端不必重复托管；若希望后端也直接服务静态资源
#（如跳过 nginx 直连调试），取消下行注释：
# ACMECAST_STATIC_DIR=/opt/acmecast/frontend
```

> 口令哈希也可用文件方式 `ACMECAST_ADMIN_PASSWORD_HASH_FILE`，该文件需运行
> 用户（`acmecast`）可读，行为见 [03-configuration.md](03-configuration.md) 3.1。

`/etc/systemd/system/acmecast.service`：

```ini
[Unit]
Description=acmecast certificate automation server
After=network-online.target
Wants=network-online.target

[Service]
User=acmecast
Group=acmecast
EnvironmentFile=/etc/acmecast/env
ExecStart=/opt/acmecast/bin/acmecast-server
Restart=on-failure
RestartSec=3

# 加固
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/opt/acmecast/data
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now acmecast
curl http://127.0.0.1:8080/healthz        # -> ok
```

后端支持 SIGTERM 优雅停机（systemd stop/restart 自动走该路径），见
[03-configuration.md](03-configuration.md) 3.5。

## 17.6 nginx 配置（合并前后端）

`/etc/nginx/sites-available/acmecast`（或 `conf.d/acmecast.conf`）：

```nginx
server {
    listen 80;
    server_name acmecast.example.com;

    # 上 TLS 后建议把 80 整段改为 301 跳转 https

    gzip on;
    gzip_types text/css application/javascript application/json image/svg+xml;

    # 前端静态资源（SPA）
    root /opt/acmecast/frontend;
    index index.html;

    # 带 hash 的构建产物可长期缓存；index.html 不缓存以保证发版即生效
    location /assets/ {
        expires 30d;
        add_header Cache-Control "public, immutable";
    }

    # API 反向代理到后端
    location /api/ {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }

    # 交互式 API 文档（不需要可删除）
    location /swagger-ui {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host $host;
    }

    location /healthz {
        proxy_pass http://127.0.0.1:8080;
    }

    # SPA 深链路由：未命中的路径回退 index.html
    location / {
        try_files $uri $uri/ /index.html;
    }
}
```

```bash
sudo ln -s /etc/nginx/sites-available/acmecast /etc/nginx/sites-enabled/
sudo nginx -t && sudo systemctl reload nginx
```

要点：

- **前端回退与 API 404 的分工**：非 `/api/` 路径由 nginx `try_files` 兜底到
  `index.html`；`/api/` 前缀整体交给后端，其内部 404 是结构化 JSON（见
  [03-configuration.md](03-configuration.md) 3.4），nginx 不要对它做 SPA 回退；
- `proxy_set_header Host $host` 必须保留——后端可能按 Host 处理请求；
- 请求体上限默认 2 MiB（`ACMECAST_BODY_LIMIT_BYTES`），nginx 的
  `client_max_body_size` 默认 1 MiB，建议在该 server 块显式设
  `client_max_body_size 4m;`，以较宽松者为准。

### 启用 HTTPS

nginx 的 TLS 证书本身也可以用 acmecast 签发后部署到本机（`cert.deploy`
`target: local`，`cert_path`/`key_path` 指向 nginx 的证书路径，
`reload_command` 填 `systemctl reload nginx`）——用
[11-frontend.md](11-frontend.md) 11.10 的流程先签一张：

```nginx
server {
    listen 443 ssl;
    http2 on;
    server_name acmecast.example.com;

    ssl_certificate     /etc/nginx/ssl/acmecast.example.com.crt;
    ssl_certificate_key /etc/nginx/ssl/acmecast.example.com.key;

    # ……其余 location 配置同上
}

server {
    listen 80;
    server_name acmecast.example.com;
    return 301 https://$host$request_uri;
}
```

## 17.7 验证

```bash
curl http://127.0.0.1:8080/healthz         # 后端直达 -> ok
curl https://acmecast.example.com/healthz  # 经 nginx -> ok
curl -sS -X POST https://acmecast.example.com/api/login \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"你的口令"}'   # -> {"data":{"token":"..."}}
```

浏览器打开 `https://acmecast.example.com/` 进入控制台；随后的凭据、流水线与
签发验证步骤与 [11-frontend.md](11-frontend.md) 11.10 的流程完全一致，
只是域名从 `localhost:5173` 换成本节的服务器地址。

## 17.8 升级

备份数据目录（迁移注意事项见
[12-operations-and-limitations.md](12-operations-and-limitations.md) 12.2）后：

```bash
cargo build --release -p acmecast-server && cd frontend && pnpm build && cd ..
sudo systemctl stop acmecast
sudo install -m 0755 target/release/acmecast-server /opt/acmecast/bin/
sudo rsync -a --delete frontend/dist/ /opt/acmecast/frontend/
sudo systemctl start acmecast
```

- 数据库迁移在启动时自动执行，失败会中止启动并报出失败版本（见
  [03-configuration.md](03-configuration.md) 3.3）；
- nginx 与前端产物均无状态，`rsync` 覆盖即可，无需 reload；
- 若给 nginx 用的证书也由 acmecast 管理，续期后流水线的
  `reload_command` 会自动 `systemctl reload nginx`。

## 17.9 排障

- **`/api` 全部 502**：后端没起来或没监听 `127.0.0.1:8080`——先
  `curl http://127.0.0.1:8080/healthz`，再看 `journalctl -u acmecast -e`；
- **后端启动即失败**：`ACMECAST_JWT_SECRET` 缺失会拒绝启动；`/etc/acmecast/env`
  权限过开时 systemd 的 `ProtectSystem` 可能拦截，确认属主 root、0600；
- **刷新页面 404**：SPA 深链未做回退，确认 `location /` 的
  `try_files $uri $uri/ /index.html;` 存在；
- **登录 500 `configuration_error`**：`ACMECAST_ADMIN_PASSWORD_HASH` 未设置或
  非法，按 17.4 重新生成并确认写进了 `EnvironmentFile`；
- **大请求被拒**：nginx `client_max_body_size`（默认 1 MiB）与后端
  `ACMECAST_BODY_LIMIT_BYTES`（默认 2 MiB）取较小者生效，按需调大。

完整排障表见 [12-operations-and-limitations.md](12-operations-and-limitations.md) 12.3。
