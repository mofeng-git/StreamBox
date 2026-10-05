#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONTEXT_DIR="$(dirname "$ROOT_DIR")"
IMAGE="${STREAMBOX_BUILDER_IMAGE:-streambox-builder:armv7}"
TARGET="armv7-unknown-linux-gnueabihf"

docker build \
  --network=host \
  --build-arg http_proxy --build-arg https_proxy --build-arg no_proxy \
  --build-arg "BASE_IMAGE=${STREAMBOX_BASE_IMAGE:-localhost/cross-rs/cross-custom-one-kvm:armv7-unknown-linux-gnueabihf-0affd}" \
  -f "$ROOT_DIR/build/Dockerfile.armv7" \
  -t "$IMAGE" \
  "$CONTEXT_DIR"

mkdir -p "$ROOT_DIR/dist"
docker run --rm \
  -e CARGO_NET_GIT_FETCH_WITH_CLI=true \
  -e TARGET="$TARGET" \
  -v "$CONTEXT_DIR:/workspace" \
  -w /workspace/StreamBox \
  "$IMAGE" \
  sh -lc 'cargo build --release --target "$TARGET" && install -Dm755 "target/$TARGET/release/streambox" dist/streambox && install -Dm644 build/streambox.service dist/streambox.service && install -Dm644 config.example.toml dist/config.example.toml'

echo "Built: $ROOT_DIR/dist/streambox"
