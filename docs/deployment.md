# Deployment

Build and run:

```bash
./build.sh
./target/release/http-tunnel-server
```

The supported runtime artifact is the binary itself. This project does not publish or document a container runtime path.
The dashboard is built as a Next.js static export and embedded into the server binary during the binary build; no Node.js runtime is required on the server host.

Without `--config`, the server uses `$HOME/.http-tunnel/server.toml`, `sqlite://$HOME/.http-tunnel/http-tunnel.sqlite3`, and `$HOME/.http-tunnel` for local data. Pass `--config` or set `HTTP_TUNNEL_CONFIG` only when a deployment needs a different location.

The public dashboard source map is optional and offline-only. Put `GeoLite2-City.mmdb` in the data directory, for example `$HOME/.http-tunnel/GeoLite2-City.mmdb`; without it, the tunnel table still works and the map shows no located points.

The server owns setup, admin, public API, tunnel WebSocket, and subdomain proxy traffic on one HTTP listener.

Example systemd unit:

```ini
[Unit]
Description=http-tunnel server
After=network-online.target

[Service]
Environment=HOME=/opt/http-tunnel
ExecStart=/opt/http-tunnel/http-tunnel-server
WorkingDirectory=/opt/http-tunnel
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
```

Set `HTTP_TUNNEL_SYSTEMD_UNIT=http-tunnel.service` or save `systemd_unit = "http-tunnel.service"` in config to allow admin restart/upgrade to request a service-manager restart. The server tries a transient `systemd-run` restart first, then direct `systemctl restart --no-block`, then a Unix exec restart of the current binary with the original command-line arguments.

Upgrade requirements:

- `release_repo` points to `owner/repo`; if omitted, the official `Xiechengqi/http-tunnel` repository is used.
- `release_tag` is `latest` or a concrete tag. Automatic upgrades only track `latest`.
- `auto_upgrade_enabled` defaults to `false`. When enabled, it controls the background 5-minute update check. Automatic replacement waits until tunnel proxy traffic has been idle for 10 seconds.
- Release asset name matches `http-tunnel-server-linux-amd64` or `http-tunnel-server-linux-arm64`.
- A SHA256 checksum asset is published as `<asset>.sha256`, `<asset>.sha256sum`, `SHA256SUMS`, `SHA256SUMS.txt`, or `checksums.txt`.
- Admin upgrade replacements reject missing or unparsable checksums.
- The downloaded binary must pass `--help` before replacement.
- The old binary is backed up as `<current_exe>.bak`.

Offline restore:

```bash
systemctl stop http-tunnel.service
./http-tunnel-server restore --backup "$HOME/.http-tunnel/backup.zip" --dry-run
./http-tunnel-server restore --backup "$HOME/.http-tunnel/backup.zip"
systemctl start http-tunnel.service
```

Restore writes the config and SQLite database paths declared by the backup config. Existing target files are copied to `.restore-bak` before replacement.

Expected release asset names:

```text
http-tunnel-server-linux-amd64
http-tunnel-client-linux-amd64
http-tunnel-server-linux-arm64
http-tunnel-client-linux-arm64
```

For aggregate checksum files, include the matching server asset filename on the same line as the 64-character SHA256 value.

Smoke test after deployment:

```bash
curl http://127.0.0.1:8080/api/v1/health
curl http://127.0.0.1:8080/api/v1/ready
http-tunnel-client connect --server http://127.0.0.1:8080 --subdomain demo --target http://127.0.0.1:3000
curl -H 'Host: demo.example.com' http://127.0.0.1:8080/
```

For Prometheus scraping, keep `metrics_public = false` unless the endpoint is already protected by the deployment boundary. Prefer scraping from a trusted peer IP or using the dedicated metrics bearer token.
