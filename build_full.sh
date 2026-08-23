#!/usr/bin/env bash
# Build the full distribution: native desktop GUI + embedded web UI.
# Note: produces the same target/release/grit path as build.sh.
set -euo pipefail
cd "$(dirname "$0")"

cargo build --release --features desktop

echo "desktop + web binary ready: target/release/grit"
