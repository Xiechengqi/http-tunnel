# Security

Admin authentication is password-only.

Admin login attempts are rate-limited per client IP to reduce brute-force risk. Tune `admin_login_failure_limit` and `admin_login_cooldown_seconds` when the defaults are too strict or too loose.

Admin actions are written to `audit_logs` with an admin token fingerprint, remote IP, action, target, result, detail, and timestamp. Audit rows are retained with the same retention window as events.

Failed admin operations are audited as first-class rows when the server can identify the attempt. This includes CSRF failures, rate-limited login attempts, weak or mismatched password changes, invalid current passwords, invalid config saves, restore validation failures, and tunnel lifecycle operations that target a missing tunnel.

Admin sessions are stored in `admin_sessions` as token hashes with remote IP, user agent, expiry, last-seen time, and optional revoked time. The server never stores bearer tokens in plaintext. Revoking an admin session invalidates both bearer-token and signed-cookie authentication for that token.

The admin config API returns a redacted config view. Password hashes, session secrets, reconnect token secrets, Turnstile secrets, metrics bearer-token hashes, and tunnel creation bearer-token hashes are preserved server-side but omitted from `GET /api/admin/config` responses. Audit details and diagnostics exports pass through the same redaction helper before they leave or enter durable operator-facing records.

Browser login sets:

- `http_tunnel_session`: httpOnly signed session cookie.
- `http_tunnel_csrf`: readable CSRF cookie used for double-submit protection.

Mutating admin APIs require `X-CSRF-Token` when authenticated by cookie. Bearer-token API clients are not subject to CSRF checks.

When `trust_proxy_headers = true`, `X-Forwarded-For` is trusted only if the direct peer IP matches `trusted_proxy_cidrs`. The default trusted CIDRs are loopback only:

```toml
trusted_proxy_cidrs = ["127.0.0.1/32", "::1/128"]
```

Set `HTTP_TUNNEL_TRUSTED_PROXY_CIDRS` to a comma-separated CIDR list when the server is behind a reverse proxy. If the proxy is not trusted, rate limiting uses the direct peer IP.

Tunnel tokens are stored hashed on the server. The client can persist tunnel credentials in:

```text
$HOME/.config/http-tunnel/client.toml
```

Set `persist_token = false` or run with `--no-persist-token` to avoid writing `tunnel_id`, `token`, and `url` to disk.

Anonymous tunnel creation can be disabled, bearer-token gated, or capped by active tunnels per IP. The admin token rotation endpoint stores only the creation-token hash; the plaintext token is returned once.

Clients can pass the creation bearer token with `--create-token`, `HTTP_TUNNEL_CREATE_TOKEN`, or `create_token` in the client config. This token authorizes tunnel creation only; existing tunnel connect/delete operations still require the per-tunnel token.

Use `POST /api/admin/tunnel-create-token/rotate` to rotate the creation token and `DELETE /api/admin/tunnel-create-token` to clear it only after public creation is enabled. Use `POST /api/admin/metrics-token/rotate` and `DELETE /api/admin/metrics-token` for the dedicated `/metrics` bearer token. Use `POST /api/admin/turnstile-secret` and `DELETE /api/admin/turnstile-secret` for Turnstile. These endpoints store only secret hashes or secret values server-side and never expose them through config reads.

Set `turnstile_secret` to require Cloudflare Turnstile verification on public tunnel creation. Clients must include a `turnstile_token` in the create request when this is enabled. `turnstile_verify_url` defaults to Cloudflare's siteverify endpoint and can be pointed at an internal test/mock verifier.

`/metrics` is not public by default. It can be accessed by an admin-authenticated request, by a direct peer whose IP matches `trusted_proxy_cidrs`, by a dedicated bearer token configured through `metrics_bearer_token_hash` or the metrics token rotation endpoint, or by explicitly setting `metrics_public = true`.

Per-tunnel access controls are enforced before a request is forwarded to a client. Tunnels can remain public, require `Authorization: Bearer <token>`, or require HTTP Basic auth. Access tokens are stored as hashes and Basic passwords are stored with the password hashing helper. Tunnels can also restrict allowed methods, block path prefixes, and set a per-minute request limit.

Inspector is disabled by default. When enabled on a tunnel, it stores bounded request/response previews in `request_inspections` and redacts sensitive headers such as `Authorization`, `Cookie`, `Set-Cookie`, `X-API-Key`, and `X-HTTP-Tunnel-Access-Token`. Replay is only allowed for inspected HTTP requests whose request body preview was not truncated.

Security headers:

- `X-Content-Type-Options: nosniff`
- `X-Frame-Options: DENY`
- `Referrer-Policy: no-referrer`
- `Permissions-Policy` disabling camera, microphone, geolocation, and payment APIs
- `Cross-Origin-Opener-Policy: same-origin`
- conservative Content Security Policy

Admin and admin API responses include `Cache-Control: no-store, max-age=0`.

When `public_scheme = "https"`, admin cookies are marked `Secure`.

Tunnel sessions use application protocol heartbeat. Non-responsive sessions are disconnected after `stale_session_seconds`, limiting stale connected state after network interruption or process death.

Tunnel sessions are not registered for proxy traffic until a valid protocol `HELLO` is accepted. Duplicate connections default to replacing the old active session and canceling its pending streams. Set `session_pool_policy = "single_reject"` to refuse a second simultaneous client, `session_pool_policy = "round_robin"` to allow multiple active clients for the same tunnel and distribute requests across them, or `session_pool_policy = "least_loaded"` to prefer the active session with the fewest in-flight streams. The legacy `duplicate_session_policy = "reject"` remains supported.
