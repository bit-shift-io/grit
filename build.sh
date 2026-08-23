#!/usr/bin/env bash
# Build the default single-binary distribution: embedded web UI only.
set -euo pipefail
cd "$(dirname "$0")"

cargo build --release

echo "web-only binary ready: target/release/grit"
