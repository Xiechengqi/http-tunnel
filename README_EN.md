<h4 align="right"><strong>English</strong> | <a href="README.md">简体中文</a></h4>

<h1 align="center">http-tunnel</h1>

<p align="center"><strong>A Rust HTTP / WebSocket tunneling system for exposing local services through public subdomains.</strong></p>

<p align="center">
  <img alt="Rust" src="https://img.shields.io/badge/Rust-async-000000?style=flat-square&logo=rust">
  <img alt="Protocol" src="https://img.shields.io/badge/HTTP%20%2F%20WebSocket-tunnel-2563eb?style=flat-square">
  <img alt="Runtime" src="https://img.shields.io/badge/runtime-binary%20only-16a34a?style=flat-square">
  <img alt="Storage" src="https://img.shields.io/badge/storage-SQLite-0f766e?style=flat-square">
</p>

`http-tunnel` ships as two binaries:

- `http-tunnel-server`: public entrypoint, admin dashboard, tunnel scheduler, request logs, and operator APIs.
- `http-tunnel-client`: runs near your local service, connects to the server, and forwards requests to the local target.

Typical flow:

```text
https://<subdomain>.<domain>
  -> http-tunnel-server
  -> persistent WebSocket tunnel
  -> http-tunnel-client
  -> http://127.0.0.1:<port>
```

The supported runtime path is binary-only. The project focuses on reliable HTTP / WebSocket tunneling, single-node self-hosted operations, and auditable administration. It does not implement raw TCP tunnels, SSH reverse forwarding, Caddy integration, OAuth, or multi-user/team management.

## Features

- Expose local HTTP services through public subdomains, including GET/POST, large request bodies, SSE streaming, and WebSocket upgrades.
- Client reconnects automatically and includes runtime state files, `status --watch`, `disconnect`, `doctor`, and pure NDJSON events for supervisors.
- `/` serves a public read-only dashboard with a tunnel table and source map for status, public URL, sessions, requests, traffic, last seen time, and expiry.
- Built-in admin dashboard for setup/login, tunnels, sessions, requests, events, audit logs, diagnostics, alerts, backup, maintenance, upgrade, and restart.
- Web UI uses Next.js static export, Tailwind CSS, shadcn/ui-style components, Tremor metric components, and Lucide/Fluent icons, then embeds the exported assets into the server binary.
- Per-tunnel access controls: public, Bearer, Basic Auth, allowed methods, blocked path prefixes, per-tunnel rate limits, and optional Inspector previews/replay.
- Session policies for replace/reject, round-robin, and least-loaded pools. Disconnect/replacement sends protocol `GOAWAY` and briefly drains active streams.
- Public tunnel creation can be disabled, protected with an admin-generated creation token, capped per IP, and optionally guarded by Cloudflare Turnstile.
- `/metrics` is protected by default and can be accessed through admin auth, trusted direct peers, a dedicated metrics bearer token, or explicit public mode.
- `/api/v1/health` is for liveness; `/api/v1/ready` checks setup completion and SQLite readiness.
- Config, diagnostics, and audit paths share redaction for passwords, secrets, tokens, and hashes.
- Upgrade resolves GitHub release assets, requires SHA256 checksum files, verifies downloads, probes `--help`, then replaces the server binary.
- Optional root-domain GitHub Proxy Server exposes GitHub downloads and raw files through `https://<domain>/gh/...`, with allow/deny rules, pass-through redirects, and optional jsDelivr raw-file rewriting.

## Quick Start

Build the static frontend and binaries:

```bash
./build.sh
```

Start the server:

```bash
./target/release/http-tunnel-server
```

Use a non-privileged port when needed:

```bash
./target/release/http-tunnel-server serve --port 8080
```

Open first-time setup:

```text
http://<server>/admin/setup
```

Set the admin password, domain, public scheme, listen address, database URL, and other runtime settings.

Create a client config:

```bash
http-tunnel-client config init
http-tunnel-client config set \
  --server https://example.com \
  --target http://127.0.0.1:3000 \
  --subdomain demo \
  --ttl-seconds 3600
```

Start the tunnel:

```bash
http-tunnel-client connect
```

CLI flags can override config values:

```bash
http-tunnel-client connect \
  --server https://example.com \
  --subdomain demo \
  --target http://127.0.0.1:3000 \
  --ttl-seconds 3600
```

`--ttl-seconds` limits this tunnel exposure window. When it expires, the server deletes the tunnel and tells the client process to exit. Changing it through config or CLI clears the saved tunnel token and creates a fresh tunnel.

Visit:

```text
https://demo.example.com
```

## Local Validation

Recommended checks before development handoff or release:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
./build.sh
```

Basic smoke:

```bash
curl http://127.0.0.1/api/v1/health
curl http://127.0.0.1/api/v1/ready
curl -H 'Host: demo.example.com' http://127.0.0.1/
```

Run the ignored non-UI smoke-soak harness manually for longer reconnect / HTTP / SSE / WebSocket coverage:

```bash
cargo test -p http-tunnel-server --test e2e_http \
  smoke_soak_harness_exercises_http_sse_websocket_and_reconnect -- --ignored
```

## Operations

The admin dashboard is available at `/admin`; first-time setup is at `/admin/setup`. After login, operators can:

- Manage tunnel lifecycle: enable, disable, extend TTL, expire now, disconnect, delete, and rotate tunnel tokens.
- View request, event, and audit logs with filters, pagination, and CSV export.
- Inspect request detail, tunnel detail, live sessions, Inspector previews, and replayable requests.
- Manage admin sessions and revoke one session or all sessions except the current one.
- Rotate or clear tunnel creation tokens, metrics tokens, and Turnstile secrets.
- Download diagnostics, review alerts, and run SQLite WAL checkpoint, ANALYZE, VACUUM, and cleanup tasks.
- Create backups, validate backups online, and restore config/database offline.
- Review release upgrade status, install SHA256-verified server binaries, and request restart; automatic upgrades wait for an idle proxy-traffic window.

## Key Configuration

By default, server config, the SQLite database, and local data files live in `$HOME/.http-tunnel`; client config and runtime files use the same directory. Common settings live in `$HOME/.http-tunnel/server.toml` and can also be overridden by CLI flags or environment variables. The public dashboard map is a country-level heat map and does not expose precise coordinates. Cloudflare proxy deployments only use trusted `CF-Connecting-IP` / `X-Forwarded-For` headers to identify the client IP. Country data only comes from the client report sent at registration and refreshed hourly; the server does not read proxy country headers or resolve countries locally from IP addresses. See [Admin](docs/admin.md) and [Security](docs/security.md) for the full operator-facing details.

| Area | Settings |
| --- | --- |
| Public entry | `domain`, `public_scheme`, `addr`, `trust_proxy_headers`, `trusted_proxy_cidrs` |
| Tunnel behavior | `tunnel_ttl_seconds`, `max_body_bytes`, `max_concurrent_streams`, `request_timeout_seconds` |
| Session pools | `session_pool_policy`, `heartbeat_interval_seconds`, `stale_session_seconds` |
| Security | `public_tunnel_create_enabled`, `tunnel_create_bearer_token_hash`, `metrics_public`, `metrics_bearer_token_hash`, `turnstile_secret` |
| Log maintenance | `request_log_retention_days`, `event_retention_days`, `session_retention_days`, `cleanup_interval_seconds` |
| Upgrade/restart | `release_repo`, `release_tag`, `github_proxy`, `auto_upgrade_enabled`, `systemd_unit` |
| GitHub Proxy Server | `github_proxy_server_enabled`, `github_proxy_server_path_prefix`, `github_proxy_server_size_limit_bytes`, `github_proxy_server_request_timeout_seconds`, `github_proxy_server_jsdelivr`, `github_proxy_server_white_list`, `github_proxy_server_black_list`, `github_proxy_server_pass_list` |

`github_proxy` is only an external proxy prefix for server self-upgrade downloads. `github_proxy_server_*` exposes this server as a root-domain GitHub proxy for external users, is disabled by default, and requires the server itself to reach GitHub directly.

### GitHub Proxy Server

When `github_proxy_server_enabled = true`, the server exposes a root-domain GitHub proxy. The default entrypoint is `/gh`:

```text
https://example.com/gh/https://github.com/owner/repo/releases/download/v1.0/app.tar.gz
https://example.com/gh/github.com/owner/repo/archive/refs/heads/main.zip
https://example.com/gh/raw.githubusercontent.com/owner/repo/main/file.txt
```

`/gh?q=https://github.com/...` redirects to the canonical `/gh/<target>` path. This feature only handles root-domain requests such as `example.com` or `www.example.com`; `/gh/...` on a tunnel subdomain still goes through normal tunnel proxy routing. `github_proxy_server_path_prefix` defaults to `/gh`, cannot use reserved prefixes such as `/api`, `/admin`, `/metrics`, or `/_next`, and requires a server restart after changes.

Supported upstreams include GitHub releases/archives, `github.com/<owner>/<repo>/blob|raw/...`, Git smart HTTP services, `raw.githubusercontent.com` files, and gist raw files. `github_proxy_server_white_list`, `github_proxy_server_black_list`, and `github_proxy_server_pass_list` accept `owner`, `owner/repo`, or `*/repo` rules. A configured allow list must match, deny-list matches are rejected, and pass-list matches redirect directly to the upstream instead of proxying through the server. When `github_proxy_server_jsdelivr = true`, raw/blob files redirect to jsDelivr.

This is a public GitHub Proxy Server capability and is separate from http-tunnel self-upgrade. If the server host cannot reach GitHub directly, keep configuring `github_proxy` with an external GitHub proxy for self-upgrade downloads.

## Documentation

- [Deployment](docs/deployment.md)
- [Client](docs/client.md)
- [Admin](docs/admin.md)
- [Cloudflare](docs/cloudflare.md)
- [Security](docs/security.md)
- [Troubleshooting](docs/troubleshooting.md)
- [Release checklist](docs/release-checklist.md)
- [Production hardening](docs/production-hardening.md)
