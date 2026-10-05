#!/usr/bin/env bash
set -euo pipefail

DIR="${1:-./data}"
exec cargo run -- --data-dir "$DIR" --bind "${STREAMBOX_BIND:-127.0.0.1}" --port "${STREAMBOX_PORT:-8090}"
