# Release Checklist

Run locally before publishing:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
./build.sh
cmp -s dashboard/src/admin.html crates/http-tunnel-server/public/admin.html
```

CI/release asset checks:

- `http-tunnel-server-linux-amd64`
- `http-tunnel-client-linux-amd64`
- `http-tunnel-server-linux-arm64`
- `http-tunnel-client-linux-arm64`
- SHA256 checksum sidecars or aggregate checksum files for server assets

These names must match the server upgrade resolver. Accepted checksum names are `<asset>.sha256`, `<asset>.sha256sum`, `SHA256SUMS`, `SHA256SUMS.txt`, and `checksums.txt`; aggregate files must include the matching server asset filename.

Deployment smoke:

- Start `http-tunnel-server serve` and confirm defaults are created under `$HOME/.http-tunnel`.
- Complete `/admin/setup`.
- Verify `/api/v1/ready` returns ready after setup and database initialization.
- Log in at `/admin/login`.
- Start a local HTTP target.
- Run `http-tunnel-client connect` from config.
- Verify HTTP GET and POST through `<subdomain>.<domain>`.
- Verify SSE streaming.
- Verify WebSocket echo.
- Trigger admin disconnect and verify client reconnect.
- Confirm `/metrics` rejects unauthenticated public access unless `metrics_public = true`, and succeeds through admin auth or the dedicated metrics bearer token.
- Run `/api/admin/upgrade/validate`.
- Run `POST /api/admin/upgrade` with `{"dry_run": true}`.
- Confirm upgrade validation or dry-run reports the expected SHA256 checksum.
- Confirm `/api/admin/config/schema` includes value types, allowed values, ranges, and restart metadata.
- Rotate and clear secret-backed settings in staging: tunnel creation token, metrics token, and Turnstile secret.
- Create a backup, run `POST /api/admin/restore/validate`, run CLI restore with `--dry-run`, and confirm the reported config/database destinations are expected.
- Run the ignored non-UI smoke-soak harness when validating release behavior under reconnects: `cargo test -p http-tunnel-server --test e2e_http smoke_soak_harness_exercises_http_sse_websocket_and_reconnect -- --ignored`.
- Walk through [Production hardening](production-hardening.md) for load, soak, restore, upgrade, and security checks.
- Confirm rollback by checking `<current_exe>.bak` behavior in a staging environment before production upgrade.

Cloudflare smoke:

- `A @` and `A *` point to the server IP.
- Both records are proxied.
- WebSocket proxying is enabled.
- Public URL uses HTTPS and `public_scheme = "https"`.
