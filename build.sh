#!/usr/bin/env bash
set -euxo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

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

rm -f -v target/release/{http-tunnel-client,http-tunnel-server}

cargo build --release -p http-tunnel-server
cargo build --release -p http-tunnel-client

cp -f -v target/release/{http-tunnel-client,http-tunnel-server} ./
