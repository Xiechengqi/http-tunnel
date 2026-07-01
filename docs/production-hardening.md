# Production Hardening

Use this checklist before operating a public instance for sustained traffic.

## Runtime

- Run the server under systemd or another process supervisor.
- Set `systemd_unit` when using systemd so admin restart and upgrade can request a managed restart.
- Keep `public_scheme = "https"` when the public edge terminates TLS.
- Configure `trusted_proxy_cidrs` before enabling `trust_proxy_headers` outside loopback.
- Wire `/api/v1/health` for liveness and `/api/v1/ready` for readiness; readiness should stay failing until setup is complete and SQLite is reachable.

## Load And Soak

- Run a multi-hour client reconnect soak with at least one HTTP target and one WebSocket target.
- Run the ignored non-UI smoke-soak harness explicitly when validating a release: `cargo test -p http-tunnel-server --test e2e_http smoke_soak_harness_exercises_http_sse_websocket_and_reconnect -- --ignored`.
- Exercise concurrent HTTP requests above normal peak traffic and confirm `max_concurrent_streams`, request timeout, and body/header limits behave as expected.
- Verify SSE streaming and WebSocket idle timeout through the actual public reverse proxy path.
- Confirm `/metrics` counters move during load and alerts stay actionable. Keep `/metrics` protected by admin auth, trusted peer CIDRs, or a dedicated metrics bearer token unless the deployment intentionally sets `metrics_public = true`.

## Backup And Restore

- Create a backup from the admin UI or CLI.
- Run `http-tunnel-server restore --backup <file> --dry-run` and verify the reported config and database destinations.
- In staging, stop the server and run restore without `--dry-run`.
- Confirm `.restore-bak` files are created when existing config/database files are replaced.

## Upgrade

- Check `GET /api/admin/upgrade/status` before replacing a binary.
- In staging, test binary replacement, restart fallback, same-version skip behavior, and rollback from `<current_exe>.bak`.
- If `auto_upgrade_enabled = true`, confirm automatic upgrade waits for the configured idle window before replacing the server binary.
- Confirm the release assets and SHA256 checksum assets use the names expected by the upgrade resolver.

## Security

- Disable public tunnel creation or gate it with a rotated creation bearer token for private deployments.
- Rotate or clear dedicated secret-backed settings through the admin secret endpoints instead of editing redacted config responses by hand.
- Set `max_active_tunnels_per_ip` for public deployments.
- Enable Turnstile if anonymous public creation remains enabled.
- Review audit logs after failed login, CSRF, config, restore, and tunnel mutation attempts.
- Keep Inspector disabled by default unless request/response previews are operationally required.
