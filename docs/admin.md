# Admin

On first launch, open `/admin/setup`, initialize the password and runtime config, then log in at `/admin/login`.

Admin auth is password-only. Login returns a bearer token for API clients and sets an httpOnly signed session cookie for the browser UI. Browser sessions and bearer tokens expire server-side after seven days.

Cookie-authenticated mutating admin APIs require `X-CSRF-Token`; the dashboard sends it automatically from the `http_tunnel_csrf` cookie. Bearer-token API clients are exempt from CSRF checks.

Admin sessions are persisted in SQLite as token hashes. This lets browser cookies survive server restarts and lets operators revoke individual sessions without storing bearer tokens in plaintext.

Useful endpoints:

```text
GET  /api/v1/ready
GET  /api/admin/status
GET  /api/admin/alerts
GET  /api/admin/diagnostics
GET  /api/admin/diagnostics/export
GET  /metrics
GET  /api/admin/config
GET  /api/admin/config/schema
PUT  /api/admin/config
POST /api/admin/config/validate
POST /api/admin/tunnel-create-token/rotate
DELETE /api/admin/tunnel-create-token
POST /api/admin/metrics-token/rotate
DELETE /api/admin/metrics-token
POST /api/admin/turnstile-secret
DELETE /api/admin/turnstile-secret
POST /api/admin/password
GET  /api/admin/sessions
POST /api/admin/sessions/:id/revoke
POST /api/admin/sessions/revoke-all
GET  /api/admin/tunnels
GET  /api/admin/tunnels/:id
GET  /api/admin/tunnels/:id/detail
PATCH /api/admin/tunnels/:id
POST /api/admin/tunnels/:id/enable
POST /api/admin/tunnels/:id/disable
POST /api/admin/tunnels/:id/disconnect
POST /api/admin/tunnels/:id/token/rotate
DELETE /api/admin/tunnels/:id
GET  /api/admin/requests
GET  /api/admin/requests/export
GET  /api/admin/requests/:id
POST /api/admin/requests/:id/replay
GET  /api/admin/events
GET  /api/admin/logs
GET  /api/admin/audit
GET  /api/admin/audit/export
POST /api/admin/cleanup
GET  /api/admin/maintenance
POST /api/admin/maintenance/wal-checkpoint
POST /api/admin/maintenance/analyze
POST /api/admin/maintenance/vacuum
POST /api/admin/backup
POST /api/admin/restore/validate
POST /api/admin/upgrade/validate
POST /api/admin/upgrade
POST /api/admin/restart
```

`GET /api/v1/ready` returns `200` when setup is complete and the database answers `SELECT 1`. It returns `503` while setup is still required or the database readiness check fails.

`GET /metrics` returns Prometheus text format with active session, active stream, tunnel status, request, byte, WebSocket session/message, disconnect reason, reconnect token, stale-session, and audit counters. It is protected by default. Access is allowed when `metrics_public = true`, when the direct peer IP matches `trusted_proxy_cidrs`, when the request is authenticated as admin, or when `Authorization: Bearer <token>` matches the dedicated `metrics_bearer_token_hash`.

`GET /api/admin/sessions` returns active and revoked admin sessions with IP, user agent, created time, expiry, and current-session marker. `POST /api/admin/sessions/revoke-all` revokes every active session except the caller.

Upgrade dry-run:

```bash
curl -X POST /api/admin/upgrade \
  -H 'content-type: application/json' \
  -d '{"dry_run": true}'
```

Dry-run resolves the release asset, fetches a SHA256 checksum, and checks local write permissions without replacing the running binary. Upgrade validation also reports available restart methods and checksum metadata when a checksum asset exists. Upgrade progress is streamed from `/api/admin/upgrade/ws` for dashboard clients.

Upgrade checksum assets must be published beside the server asset. The resolver accepts `<asset>.sha256`, `<asset>.sha256sum`, `SHA256SUMS`, `SHA256SUMS.txt`, or `checksums.txt`. Aggregate checksum files must contain a 64-character SHA256 value and the matching server asset filename. `POST /api/admin/upgrade` rejects dry-runs and replacements when a checksum cannot be found or parsed, and verifies the downloaded binary before the `--help` probe and replacement.

`pending_restart` is persisted when restart-required config fields change, including listen address, domain, public scheme, database URL, and data dir.

`GET /api/admin/config` returns a safe config view. It does not expose password hashes, admin session secrets, reconnect token secrets, Turnstile secrets, metrics bearer-token hashes, or tunnel creation bearer-token hashes. Config save and validation preserve existing secrets when these fields are omitted.

`GET /api/admin/config/schema` returns editable config field metadata, including value type, sensitivity, required flag, restart requirement, hot-reloadability, default value, allowed values, numeric range, and operational notes.

`GET /api/admin/diagnostics` returns an operator diagnostics package containing admin status, redacted config, config schema metadata, alerts, maintenance state, build info, and a compact metrics summary. `GET /api/admin/diagnostics/export` returns the same redacted package as a JSON attachment. Secret-bearing fields and sensitive audit details are redacted the same way as `GET /api/admin/config`.

List endpoints support pagination with `limit` and `offset`; `limit` is clamped to 500. JSON response bodies keep their existing shape, and pagination metadata is returned in headers:

- `x-http-tunnel-total-count`
- `x-http-tunnel-limit`
- `x-http-tunnel-offset`
- `x-http-tunnel-has-more`

Supported filters:

- `GET /api/admin/tunnels`: `status`, `subdomain`, `q`
- `GET /api/admin/requests`: `type`, `status`, `error_only`, `q`
- `GET /api/admin/events`: `kind`, `q`
- `GET /api/admin/logs`: `kind`, `error_only`, `q`
- `GET /api/admin/audit`: `action`, `result`, `target_type`, `q`

`GET /api/admin/requests/export` and `GET /api/admin/audit/export` accept the same filters as their JSON list endpoints and return a CSV attachment. By default they export the current `limit`/`offset` window. Passing `all=true` exports the filtered result set from offset zero with a hard cap of 10,000 rows. CSV responses include `x-http-tunnel-export-row-count`, `x-http-tunnel-export-total-count`, and `x-http-tunnel-export-truncated`.

`GET /api/admin/alerts` returns query-derived health alerts for connected tunnels without runtime sessions, disabled/offline/expired tunnel counts, recent request errors, recent 5xx responses, abnormal WebSocket close codes, and stale runtime sessions.

`GET /api/admin/tunnels/:id/detail` returns the tunnel record, latest session, active runtime sessions, request/error counts, recent requests, recent events, and the last request error.

`GET /api/admin/requests/:id` returns one request log entry with full request metadata plus associated tunnel/session summaries. When Inspector was enabled on the tunnel, the response includes bounded request/response header and body previews. Sensitive headers are redacted.

`POST /api/admin/requests/:id/replay` safely replays an inspected HTTP request through an active tunnel session and returns the replay request ID, original `replay_of` ID, status, response headers, and a bounded body preview. Optional JSON overrides can replace `method`, `path`, `headers`, and `body`; truncated original request bodies remain blocked unless a replacement body is supplied.

`PATCH /api/admin/tunnels/:id` updates lifecycle and policy fields. Supported JSON fields include `ttl_seconds`, `expire_now`, `enabled`, `access_policy`, `access_token`, `access_username`, `access_password`, `allowed_methods`, `blocked_path_prefixes`, `inspector_enabled`, and `rate_limit_per_minute`. TTL values are applied from the current time. `access_policy` accepts `public`, `bearer`, or `basic`.

`POST /api/admin/tunnels/:id/token/rotate` returns a new tunnel token, stores only its hash, disconnects the active client session, and invalidates the old token for future connects/deletes.

Failed admin mutations are also audited. This includes CSRF failures, rate-limited login attempts, weak or mismatched password changes, invalid config saves, restore validation failures, and tunnel lifecycle operations against missing tunnel IDs.

Public tunnel creation can be hardened with:

- `public_tunnel_create_enabled`
- an admin-generated creation bearer token, stored server-side as a hash
- `max_active_tunnels_per_ip`
- optional Turnstile validation with `turnstile_secret`; `turnstile_verify_url` defaults to Cloudflare and can point to a test/mock verifier

`POST /api/admin/tunnel-create-token/rotate` returns a new creation bearer token once and stores only its hash. `DELETE /api/admin/tunnel-create-token` clears the stored hash only when public tunnel creation is enabled; this prevents private deployments from clearing the token required to create tunnels. When public creation is disabled, clients must pass this token as `Authorization: Bearer <token>` to `POST /api/v1/tunnels`.

`POST /api/admin/metrics-token/rotate` returns a new dedicated `/metrics` bearer token once and stores only its hash. `DELETE /api/admin/metrics-token` clears it. `POST /api/admin/turnstile-secret` accepts `{"secret":"..."}` and stores the Turnstile secret; `DELETE /api/admin/turnstile-secret` clears it. These endpoints write audit rows and avoid sending stored secret material back through `GET /api/admin/config`.

`POST /api/admin/cleanup` runs cleanup immediately and reports expired tunnel counts plus deleted request/event/audit/session rows. Retention and cleanup interval are controlled by:

- `cleanup_interval_seconds`
- `request_log_retention_days`
- `event_retention_days`
- `session_retention_days`

Maintenance endpoints report the live SQLite database path, DB/WAL/SHM sizes, table counts, and active runtime session count. Mutating maintenance actions run `wal_checkpoint(TRUNCATE)`, `ANALYZE`, or `VACUUM` and write audit records.

Backup and restore:

```bash
http-tunnel-server backup --config ./data/server.toml --output ./data/backup.zip
http-tunnel-server restore --backup ./data/backup.zip --dry-run
http-tunnel-server restore --config ./data/server.toml --backup ./data/backup.zip
```

The backup archive includes `manifest.json`, `config/server.toml`, the SQLite database plus WAL/SHM files when present, and build info. The admin API `POST /api/admin/backup` returns the same ZIP format. `POST /api/admin/restore/validate` accepts `{"path":"..."}` and returns `validation` plus a dry-run `restore_plan` with target config/database paths, companion WAL/SHM paths, overwritten files, stale companion files that would be removed, and restore warnings. Actual restore is intentionally a CLI/offline operation: stop the server first, run `restore`, then start the binary again. Existing target files are copied to `.restore-bak` paths before replacement.

The database records applied schema versions in `schema_migrations`; current fresh installs record `0001_init`.

The admin dashboard is organized into Overview, Tunnels, Activity, Security, Config, Maintenance, and Version tabs. It can manage runtime config, apply local schema-based validation before save, view schema metadata, copy or download diagnostics, view alerts, filter/paginate tunnels and logs, export request/audit CSV files for the current page or filtered result set, inspect tunnel detail, toggle Inspector, edit tunnel access/rules with a form, view audit logs, manage admin sessions, rotate tunnel tokens, rotate or clear secret-backed config values, extend/expire/enable/disable/disconnect/delete tunnels, run cleanup and DB maintenance, download backups, validate backup archives, change the admin password, validate upgrade settings, and request restart/upgrade. Request logs show request ID, type, method, path, host, remote IP, user agent, status, duration, byte counts, errors, replay lineage, WebSocket close/message metadata, and optional Inspector previews.

Public tunnel deletion through `DELETE /api/v1/tunnels/:id` requires the tunnel token via `Authorization: Bearer <token>` or `?token=`.

Expired tunnels return `410 Gone` with `x-http-tunnel-reason: tunnel_expired` for public proxy traffic and tunnel WebSocket connects.

Tunnel sessions use protocol-level heartbeat. The server sends `Ping` frames every `heartbeat_interval_seconds`; clients reply with `Pong`. Sessions older than `stale_session_seconds` without activity are disconnected and marked with `disconnect_reason = "stale_session"`.

Tunnel WebSocket sessions are registered for public proxy traffic only after a valid `HELLO`. Duplicate client sessions for the same subdomain use `session_pool_policy`: `single_replace` preserves the default replacement behavior, `single_reject` refuses the second session, `round_robin` dispatches requests across active sessions, and `least_loaded` prefers the runtime session with the fewest active streams. On admin disconnect or replacement, the server sends protocol `GOAWAY`, waits briefly for in-flight streams to drain, and then cancels remaining streams. Modern clients stop accepting new streams after `GOAWAY` while allowing active HTTP/WebSocket streams to finish. Tunnel detail exposes per-session runtime stream, byte, selection, and last-selected metrics. The legacy `duplicate_session_policy = "reject"` still maps to `single_reject`.

The server issues short-lived reconnect tokens in `HELLO_ACK`; modern clients present them in the next `HELLO` after a disconnect. These tokens are signed, expire after five minutes, and are used for reconnect diagnostics without replacing the tunnel token required by the WebSocket connect URL.

Tunnel detail includes client protocol metadata when the client sends it: client version, protocol version compatibility, capabilities, and remote address. Legacy clients that omit protocol metadata are accepted; clients that explicitly send an unsupported protocol version are disconnected.
