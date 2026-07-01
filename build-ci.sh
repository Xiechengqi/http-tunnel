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

VERSION="$(grep '^version = ' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/' || true)"
COMMIT="$(git rev-parse --short=7 HEAD 2>/dev/null || echo unknown)"
BUILD_TIME="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"

if [ -f dashboard/package-lock.json ]; then
  npm --prefix dashboard ci
else
  npm --prefix dashboard install
fi

HTTP_TUNNEL_VERSION="$VERSION" \
HTTP_TUNNEL_COMMIT="$COMMIT" \
HTTP_TUNNEL_BUILD_TIME="$BUILD_TIME" \
  npm --prefix dashboard run build

if command -v cargo-zigbuild >/dev/null 2>&1; then
  cargo zigbuild --release --target "$RUST_TARGET" -p http-tunnel-server
  cargo zigbuild --release --target "$RUST_TARGET" -p http-tunnel-client
else
  cargo build --release --target "$RUST_TARGET" -p http-tunnel-server
  cargo build --release --target "$RUST_TARGET" -p http-tunnel-client
fi
