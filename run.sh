#!/usr/bin/env bash
# Run the web-only daemon (builds it first if missing).
# Extra args are forwarded to grit, e.g.: ./run.sh --port 8080 --path /repo
set -euo pipefail
cd "$(dirname "$0")"

BIN=target/release/grit
if [[ ! -x "$BIN" ]]; then
  ./build.sh
fi

exec "$BIN" "$@"
