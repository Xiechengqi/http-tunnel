# Deployment

Build and run:

```bash
./build.sh
./target/release/http-tunnel-server serve --config ./data/server.toml
```

The supported runtime artifact is the binary itself. This project does not publish or document a container runtime path.

The server owns setup, admin, public API, tunnel WebSocket, and subdomain proxy traffic on one HTTP listener.

Example systemd unit:

```ini
[Unit]
Description=http-tunnel server
After=network-online.target

[Service]
ExecStart=/opt/http-tunnel/http-tunnel-server serve --config /opt/http-tunnel/data/server.toml
WorkingDirectory=/opt/http-tunnel
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
```

Set `HTTP_TUNNEL_SYSTEMD_UNIT=http-tunnel.service` or save `systemd_unit = "http-tunnel.service"` in config to allow admin restart/upgrade to request a service-manager restart. The server tries a transient `systemd-run` restart first, then direct `systemctl restart --no-block`, then a Unix exec restart of the current binary with the original command-line arguments.

Upgrade requirements:

- `release_repo` points to `owner/repo`; leaving it empty disables admin upgrade checks and binary replacement.
- `release_tag` is `latest` or a concrete tag.
- Release asset name matches `http-tunnel-server-linux-amd64` or `http-tunnel-server-linux-arm64`.
- A SHA256 checksum asset is published as `<asset>.sha256`, `<asset>.sha256sum`, `SHA256SUMS`, `SHA256SUMS.txt`, or `checksums.txt`.
- Admin upgrade dry-runs and replacements reject missing or unparsable checksums.
- The downloaded binary must pass `--help` before replacement.
- The old binary is backed up as `<current_exe>.bak`.

Offline restore:

```bash
systemctl stop http-tunnel.service
./http-tunnel-server restore --config ./data/server.toml --backup ./data/backup.zip --dry-run
./http-tunnel-server restore --config ./data/server.toml --backup ./data/backup.zip
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
