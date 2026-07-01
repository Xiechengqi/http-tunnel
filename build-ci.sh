#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

TARGET="${1:-amd64}"
case "$TARGET" in
  amd64) RUST_TARGET="x86_64-unknown-linux-musl" ;;
  arm64) RUST_TARGET="aarch64-unknown-linux-musl" ;;
  *) echo "Usage: $0 [amd64|arm64]" >&2; exit 1 ;;
esac

mkdir -p crates/http-tunnel-server/public
cp -R dashboard/src/. crates/http-tunnel-server/public/
printf '{"version":"%s","commit":"%s","buildTime":"%s"}\n' \
  "$(grep '^version = ' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/' || true)" \
  "$(git rev-parse --short=7 HEAD 2>/dev/null || echo unknown)" \
  "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" \
  > crates/http-tunnel-server/public/build-info.json

if command -v cargo-zigbuild >/dev/null 2>&1; then
  cargo zigbuild --release --target "$RUST_TARGET" -p http-tunnel-server
  cargo zigbuild --release --target "$RUST_TARGET" -p http-tunnel-client
else
  cargo build --release --target "$RUST_TARGET" -p http-tunnel-server
  cargo build --release --target "$RUST_TARGET" -p http-tunnel-client
fi
