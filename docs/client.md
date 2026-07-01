# Client

Initialize config:

```bash
http-tunnel-client config init
```

Default path:

```text
$HOME/.config/http-tunnel/client.toml
```

Example:

```toml
server = "https://example.com"
subdomain = "demo"
target = "http://127.0.0.1:3000"
persist_token = true
# optional when public tunnel creation is disabled:
# create_token = "..."
# written automatically when persist_token = true:
# tunnel_id = "tun_..."
# token = "..."
# url = "https://demo.example.com"
```

Run from config:

```bash
http-tunnel-client connect
```

Inspect or update config:

```bash
http-tunnel-client config show
http-tunnel-client config set --server https://example.com --target http://127.0.0.1:3000 --subdomain demo
http-tunnel-client config set --tunnel-id tun_123 --token <token> --url https://demo.example.com
http-tunnel-client config set --create-token <admin-generated-create-token>
http-tunnel-client config set --persist-token false
http-tunnel-client config clear-token
```

Run diagnostics before connecting:

```bash
http-tunnel-client doctor
http-tunnel-client doctor --json
http-tunnel-client doctor --server https://example.com --target http://127.0.0.1:3000
http-tunnel-client doctor --websocket-path /ws
```

`doctor` checks config loading, server health, server protocol version, local target reachability, optional target WebSocket reachability, subdomain state, stored tunnel credential completeness, and stored tunnel server state. It exits non-zero when any check is an error.

CLI flags override config:

```bash
http-tunnel-client connect \
  --server https://example.com \
  --subdomain demo \
  --target http://127.0.0.1:3000 \
  --create-token <admin-generated-create-token>
```

Use `--json-events` with `connect` or `http` to print newline-delimited JSON runtime events for supervisors or log processors. In this mode stdout is pure NDJSON: human-readable startup and reconnect messages are suppressed so every non-empty stdout line is parseable JSON. Events include `startup`, `connected`, `disconnected`, `reconnecting`, `connection_failed`, `interrupted`, and `exit`.

Runtime status and local disconnect control:

```bash
http-tunnel-client status
http-tunnel-client status --watch
http-tunnel-client disconnect
http-tunnel-client disconnect --timeout 10
http-tunnel-client runtime clean
```

`status` prints `$HOME/.config/http-tunnel/runtime.json` as JSON. It includes PID, server, target, tunnel ID, public URL, connected state, stale PID detection, active stream count, byte counters, last disconnect reason, and update time. `--watch` repeats the status output at the requested interval. `disconnect` writes a local stop flag and waits up to the timeout for the running client to exit gracefully without a local daemon socket. `runtime clean` removes stale runtime status and disconnect flag files; it refuses to clean an apparently live client unless `--force` is supplied.

Convenience local-port command:

```bash
http-tunnel-client http 3000 --server https://example.com --subdomain demo
```

Release a stored tunnel and clear local credentials:

```bash
http-tunnel-client release
http-tunnel-client release --server https://example.com --tunnel-id tun_123 --token <tunnel-token>
```

The client creates a tunnel, connects over WebSocket, forwards HTTP/WebSocket traffic to the local target, and reconnects with backoff after disconnects.

On connect, the client sends a protocol `Hello` containing the client version, protocol version, capabilities (`http`, `websocket`, `heartbeat`), and any in-memory reconnect token from the previous connection. The server accepts legacy clients that do not send these fields, but disconnects clients that explicitly report an unsupported protocol version.

The client responds to protocol `Ping` frames with `Pong`; this lets the server detect half-open or stale sessions.

On disconnect, the client prints active stream count, transferred bytes, and the last disconnect reason before reconnecting. In JSON event mode the same data is emitted as structured JSON events.

When `persist_token = true`, the client writes `tunnel_id`, `token`, and `url` back to the config file and reuses them on the next start. Use `--no-persist-token`, `persist_token = false`, or `http-tunnel-client config clear-token` to avoid or clear stored tunnel credentials.

If `--server` or `--subdomain` explicitly changes the saved endpoint, the client clears the stored tunnel token before connecting so it does not accidentally reuse credentials for a different tunnel.

If a stored tunnel returns a terminal error such as unauthorized, not found, forbidden, or expired, the client clears it and creates a fresh tunnel when token persistence is enabled.

When public tunnel creation is disabled server-side, pass the admin-generated creation bearer token with `--create-token`, `HTTP_TUNNEL_CREATE_TOKEN`, or `create_token` in the client config. The creation token is only used for `POST /api/v1/tunnels`; the tunnel token is still required for connecting or releasing a specific tunnel.
