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
  --subdomain demo
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
  --target http://127.0.0.1:3000
```

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

By default, server config, the SQLite database, and local data files live in `$HOME/.http-tunnel`; client config and runtime files use the same directory. Common settings live in `$HOME/.http-tunnel/server.toml` and can also be overridden by CLI flags or environment variables. The public dashboard map is a country-level heat map and does not expose precise coordinates. Cloudflare proxy deployments use trusted `CF-Connecting-IP` and `CF-IPCountry` headers; other deployments can place `GeoIP-Country.mmdb` at `$HOME/.http-tunnel/GeoIP-Country.mmdb`. See [Admin](docs/admin.md) and [Security](docs/security.md) for the full operator-facing details.

| Area | Settings |
| --- | --- |
| Public entry | `domain`, `public_scheme`, `addr`, `trust_proxy_headers`, `trusted_proxy_cidrs` |
| Tunnel behavior | `tunnel_ttl_seconds`, `max_body_bytes`, `max_concurrent_streams`, `request_timeout_seconds` |
| Session pools | `session_pool_policy`, `heartbeat_interval_seconds`, `stale_session_seconds` |
| Security | `public_tunnel_create_enabled`, `tunnel_create_bearer_token_hash`, `metrics_public`, `metrics_bearer_token_hash`, `turnstile_secret` |
| Log maintenance | `request_log_retention_days`, `event_retention_days`, `session_retention_days`, `cleanup_interval_seconds` |
| Upgrade/restart | `release_repo`, `release_tag`, `auto_upgrade_enabled`, `systemd_unit` |

## Documentation

- [Deployment](docs/deployment.md)
- [Client](docs/client.md)
- [Admin](docs/admin.md)
- [Cloudflare](docs/cloudflare.md)
- [Security](docs/security.md)
- [Troubleshooting](docs/troubleshooting.md)
- [Release checklist](docs/release-checklist.md)
- [Production hardening](docs/production-hardening.md)
