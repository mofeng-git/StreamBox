#!/usr/bin/env bash
set -euo pipefail

TARGET="armv7-unknown-linux-gnueabihf"
LIBYUV_STATIC=1 cargo build --release --target "$TARGET"
install -Dm755 "target/$TARGET/release/streambox" "${DESTDIR:-dist}/streambox"
install -Dm644 build/streambox.service "${DESTDIR:-dist}/streambox.service"
install -Dm644 config.example.toml "${DESTDIR:-dist}/config.example.toml"
