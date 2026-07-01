#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

mkdir -p crates/http-tunnel-server/public
cp -R dashboard/src/. crates/http-tunnel-server/public/
printf '{"version":"%s","commit":"%s","buildTime":"%s"}\n' \
  "$(grep '^version = ' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/' || true)" \
  "$(git rev-parse --short=7 HEAD 2>/dev/null || echo unknown)" \
  "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" \
  > crates/http-tunnel-server/public/build-info.json

cargo build --release -p http-tunnel-server
cargo build --release -p http-tunnel-client
