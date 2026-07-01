# Troubleshooting

Common public tunnel responses:

- `404 tunnel_not_found`: host is not under the configured domain, or the subdomain does not exist.
- `403 tunnel_disabled`: the tunnel was disabled from admin.
- `503 tunnel_offline`: the tunnel exists but no client is connected.
- `502 local_target_failed`: the client is connected but the local target failed or refused WebSocket upgrade.
- `504 tunnel_timeout`: the tunnel did not respond before `request_timeout_seconds`.

Health and readiness:

- `/api/v1/health` only confirms the process can answer HTTP.
- `/api/v1/ready` returns `503` while setup is incomplete or SQLite fails a readiness query.
- If readiness fails after setup, check the configured `database_url`, database file permissions, and WAL/SHM path permissions.

Metrics:

- `/metrics` returns `401` by default for untrusted unauthenticated callers.
- Authenticate as admin, scrape from a direct peer IP in `trusted_proxy_cidrs`, configure a dedicated metrics bearer token, or intentionally set `metrics_public = true`.

Client reconnect:

- Run `http-tunnel-client doctor` to check config, server health, target reachability, and stored tunnel credentials before starting a tunnel.
- The client reconnects with backoff after disconnects.
- If persisted tunnel credentials fail, the client creates a fresh tunnel and updates config when `persist_token = true`.
- If a tunnel repeatedly changes to `stale_session`, check network stability, reverse proxy WebSocket idle timeouts, and `heartbeat_interval_seconds` / `stale_session_seconds`.

Cloudflare:

- Ensure wildcard DNS is proxied.
- Ensure WebSocket support is enabled.
- Use `public_scheme = "https"` for public Cloudflare HTTPS.
- If the dashboard country map is empty or points to the wrong country, confirm `trusted_proxy_cidrs` includes Cloudflare ranges so `CF-Connecting-IP` and `CF-IPCountry` are trusted.

Upgrade rollback:

- Upgrade backs up the current binary as `<current_exe>.bak`.
- Check `GET /api/admin/upgrade/status` before replacing binaries.
- If upgrade fails with `upgrade_checksum_missing`, publish `<asset>.sha256`, `<asset>.sha256sum`, `SHA256SUMS`, `SHA256SUMS.txt`, or `checksums.txt` with the matching server asset SHA256.
- If replacement fails, the backup is copied back over the current executable.
- Restart after upgrade is attempted through `systemd-run`, `systemctl`, then Unix exec. If all methods fail, start the backed-up or replaced binary manually under the same supervisor.

Restore:

- Always run `http-tunnel-server restore --backup <file> --dry-run` first.
- Stop the server before running restore without `--dry-run`; online restore is deliberately not exposed through the admin API.
- If restore writes the wrong destination, inspect the restored config's `database_url`; restore uses the database path declared inside the backup config.
- Existing files are copied to `.restore-bak` before replacement.
