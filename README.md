<h4 align="right"><a href="README_EN.md">English</a> | <strong>简体中文</strong></h4>

<p align="center">
  <img src="dashboard/app/icon.png" alt="http-tunnel icon" width="72" height="72">
</p>

<h1 align="center">http-tunnel</h1>

<p align="center"><strong>一个用 Rust 编写的 HTTP / WebSocket 隧道系统，将本地服务安全暴露到公网子域名。</strong></p>

<p align="center">
  <img alt="Rust" src="https://img.shields.io/badge/Rust-async-000000?style=flat-square&logo=rust">
  <img alt="Protocol" src="https://img.shields.io/badge/HTTP%20%2F%20WebSocket-tunnel-2563eb?style=flat-square">
  <img alt="Runtime" src="https://img.shields.io/badge/runtime-binary%20only-16a34a?style=flat-square">
  <img alt="Storage" src="https://img.shields.io/badge/storage-SQLite-0f766e?style=flat-square">
</p>

`http-tunnel` 由两个二进制组成：

- `http-tunnel-server`：公网入口、管理后台、隧道调度、请求日志和运维 API。
- `http-tunnel-client`：运行在本地网络中，连接 server 并把请求转发到本地目标服务。

典型链路：

```text
https://<subdomain>.<domain>
  -> http-tunnel-server
  -> persistent WebSocket tunnel
  -> http-tunnel-client
  -> http://127.0.0.1:<port>
```

项目只支持二进制运行路径，不维护容器镜像或编排运行方式。当前目标是可靠的 HTTP / WebSocket 隧道、单机自托管运维和可审计的管理能力；不实现原始 TCP 隧道、SSH 反向转发、Caddy 集成、OAuth、多用户/团队系统。

## 特性

- 通过公网子域名访问本地 HTTP 服务，支持 GET/POST、大请求体、SSE 流式响应和 WebSocket 升级。
- 客户端自动重连，支持本地运行态文件、`status --watch`、`disconnect`、`doctor` 和纯 NDJSON 事件输出，方便交给进程管理器托管。
- `/` 提供公开只读 dashboard，以表格和来源地图展示隧道状态、公开地址、会话、请求、流量、最近活跃和过期时间。
- 管理后台内置 setup/login、隧道列表、会话管理、请求/事件/审计日志、诊断包、告警、备份、维护、升级和重启入口。
- Web UI 使用 Next.js static export、Tailwind CSS、shadcn/ui 风格组件、Tremor 指标组件和 Lucide/Fluent 图标，构建后嵌入 server binary。
- 支持隧道级访问控制：公开访问、Bearer、Basic Auth、方法白名单、路径前缀阻断、每隧道限流和可选 Inspector 请求预览/重放。
- 支持单连接替换/拒绝、轮询和 least-loaded 多客户端会话池，并在断开或替换前发送协议级 `GOAWAY` 以尽量排空进行中的请求。
- 公开创建隧道可关闭、可使用管理员生成的创建令牌保护、可按 IP 限制活跃隧道数量，并可接入 Cloudflare Turnstile。
- `/metrics` 默认受保护，支持管理员认证、可信直连来源、专用 metrics bearer token 或显式公开。
- `/api/v1/health` 用于存活检查，`/api/v1/ready` 用于 setup 完成和 SQLite 可用性检查。
- 配置、诊断和审计路径统一遮蔽密码、secret、token 和 hash 等敏感信息。
- 升级流程解析 GitHub release 资产，要求 SHA256 校验文件，下载后先校验再执行 `--help` 探测和二进制替换。

## 快速开始

先构建前端静态资源并编译二进制：

```bash
./build.sh
```

启动 server：

```bash
./target/release/http-tunnel-server
```

如需使用非特权端口运行：

```bash
./target/release/http-tunnel-server serve --port 8080
```

首次启动后打开：

```text
http://<server>/admin/setup
```

完成管理员密码、域名、公网协议、监听地址、数据库地址等初始化配置。

创建客户端配置：

```bash
http-tunnel-client config init
http-tunnel-client config set \
  --server https://example.com \
  --target http://127.0.0.1:3000 \
  --subdomain demo
```

启动隧道：

```bash
http-tunnel-client connect
```

也可以不写配置，直接用命令行参数覆盖：

```bash
http-tunnel-client connect \
  --server https://example.com \
  --subdomain demo \
  --target http://127.0.0.1:3000
```

访问：

```text
https://demo.example.com
```

## 本地验证

开发和发布前建议执行：

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
./build.sh
```

基础 smoke：

```bash
curl http://127.0.0.1/api/v1/health
curl http://127.0.0.1/api/v1/ready
curl -H 'Host: demo.example.com' http://127.0.0.1/
```

长时间 reconnect / HTTP / SSE / WebSocket 行为可以手动运行被忽略的非 UI smoke-soak harness：

```bash
cargo test -p http-tunnel-server --test e2e_http \
  smoke_soak_harness_exercises_http_sse_websocket_and_reconnect -- --ignored
```

## 运维能力

管理后台默认位于 `/admin`，首次初始化位于 `/admin/setup`。登录后可以完成：

- 管理隧道生命周期：启用、禁用、延长 TTL、立即过期、断开、删除、轮换隧道 token。
- 查看请求、事件、审计日志，按条件过滤分页，并导出 CSV。
- 查看请求详情、隧道详情、运行中 session、Inspector 预览和可重放请求。
- 管理管理员 session，撤销单个 session 或撤销除当前外的全部 session。
- 轮换或清理创建隧道 token、metrics token、Turnstile secret。
- 下载诊断包，查看告警，执行 SQLite WAL checkpoint、ANALYZE、VACUUM 和清理任务。
- 创建备份，在线校验备份，离线恢复配置和数据库。
- 查看 release 升级状态，下载带 SHA256 校验的 server 二进制并请求重启；自动升级会等待代理流量空闲窗口。

## 关键配置

默认情况下，server 配置、SQLite 数据库和本地数据文件会保存在 `$HOME/.http-tunnel`，client 配置和运行态文件也使用同一个目录。常见配置项会保存到 `$HOME/.http-tunnel/server.toml`，也可通过命令行参数或环境变量覆盖。公开 dashboard 的地图使用国家级热力图，不暴露精确坐标；Cloudflare proxy 场景会读取可信代理传入的 `CF-Connecting-IP` 和 `CF-IPCountry`，其他场景可将 `GeoIP-Country.mmdb` 放到 `$HOME/.http-tunnel/GeoIP-Country.mmdb`。更完整的字段说明见 [Admin 文档](docs/admin.md) 和 [Security 文档](docs/security.md)。

| 领域 | 配置 |
| --- | --- |
| 公网入口 | `domain`、`public_scheme`、`addr`、`trust_proxy_headers`、`trusted_proxy_cidrs` |
| 隧道行为 | `tunnel_ttl_seconds`、`max_body_bytes`、`max_concurrent_streams`、`request_timeout_seconds` |
| 会话池 | `session_pool_policy`、`heartbeat_interval_seconds`、`stale_session_seconds` |
| 安全 | `public_tunnel_create_enabled`、`tunnel_create_bearer_token_hash`、`metrics_public`、`metrics_bearer_token_hash`、`turnstile_secret` |
| 日志维护 | `request_log_retention_days`、`event_retention_days`、`session_retention_days`、`cleanup_interval_seconds` |
| 升级重启 | `release_repo`、`release_tag`、`auto_upgrade_enabled`、`systemd_unit` |

## 文档

- [部署](docs/deployment.md)
- [客户端](docs/client.md)
- [管理后台](docs/admin.md)
- [Cloudflare](docs/cloudflare.md)
- [安全](docs/security.md)
- [故障排查](docs/troubleshooting.md)
- [发布检查清单](docs/release-checklist.md)
- [生产加固](docs/production-hardening.md)
