#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORKTREE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BUILD_DIR="$SCRIPT_DIR/.build"
BINARY_OUT="$BUILD_DIR/ns2"
DOCKERFILE="$SCRIPT_DIR/Dockerfile"
DOCKERFILE_BUILDER="$SCRIPT_DIR/Dockerfile.builder"

# ── Builder image (Rust + system deps for compilation) ────────────────────────
needs_build() {
    local image="$1" dockerfile="$2"
    local created
    created="$(docker inspect --format '{{.Created}}' "$image" 2>/dev/null || true)"
    [[ -z "$created" ]] && return 0
    local image_epoch dockerfile_epoch
    image_epoch="$(date -d "$created" +%s 2>/dev/null \
        || date -j -f '%Y-%m-%dT%H:%M:%S' "${created%%.*}" +%s 2>/dev/null \
        || echo 0)"
    if stat --version &>/dev/null 2>&1; then
        dockerfile_epoch="$(stat -c %Y "$dockerfile")"
    else
        dockerfile_epoch="$(stat -f %m "$dockerfile")"
    fi
    [[ "$dockerfile_epoch" -gt "$image_epoch" ]]
}

if needs_build ns2-builder "$DOCKERFILE_BUILDER"; then
    echo "Building ns2-builder image..."
    docker build -t ns2-builder -f "$DOCKERFILE_BUILDER" "$SCRIPT_DIR/"
else
    echo "ns2-builder image is up to date."
fi

# ── Compile inside the builder container ──────────────────────────────────────
# Uses a named volume for the Cargo registry so incremental builds are fast.
# Output goes to target/linux/ to avoid colliding with host native builds.
echo "Compiling ns2 for Linux..."
docker volume create ns2-cargo-registry >/dev/null 2>&1 || true
docker run --rm \
    -v "$WORKTREE_DIR":/workspace \
    -v ns2-cargo-registry:/usr/local/cargo/registry \
    -w /workspace \
    ns2-builder \
    cargo build --release --target-dir target/linux

mkdir -p "$BUILD_DIR"
ln -sf "$WORKTREE_DIR/target/linux/release/ns2" "$BINARY_OUT"
echo "Binary: $BINARY_OUT"

# ── Test runtime image ────────────────────────────────────────────────────────
if needs_build ns2-test "$DOCKERFILE"; then
    echo "Building ns2-test image..."
    docker build -t ns2-test "$SCRIPT_DIR/"
else
    echo "ns2-test image is up to date."
fi

echo ""
echo "Done. Binary: $BINARY_OUT  Images: ns2-builder ns2-test"
