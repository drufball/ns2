#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

mkdir -p "$SCRIPT_DIR/.build"

echo "==> Compiling ns2 for Linux using Docker..."

docker build \
  -t ns2-builder:latest \
  -f - "$REPO_ROOT" <<'DOCKERFILE'
FROM rust:1.86-bookworm
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN cargo build --release --bin ns2
DOCKERFILE

CONTAINER_ID=$(docker create ns2-builder:latest)
docker cp "$CONTAINER_ID:/build/target/release/ns2" "$SCRIPT_DIR/.build/ns2"
docker rm "$CONTAINER_ID"

echo "==> Binary extracted to product-flows/.build/ns2"

echo "==> Building ns2-test Docker image..."
docker build -f "$SCRIPT_DIR/Dockerfile" -t ns2-test "$SCRIPT_DIR"

echo "==> Build complete."
echo "    Binary: product-flows/.build/ns2"
echo "    Image:  ns2-test"
