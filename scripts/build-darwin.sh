#!/usr/bin/env bash
# Build Klepto release binaries for macOS.
# Requires a macOS host (Apple SDK). Cross-compiles amd64 on Apple Silicon.
#
# Usage: ./scripts/build-darwin.sh [amd64|arm64|all]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
KLEPTO_DIR="$ROOT/klepto"
DIST_DIR="$ROOT/dist"
MODE="${1:-all}"

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: '$1' is required" >&2
    exit 1
  }
}

ensure_target() {
  rustup target add "$1" >/dev/null
}

write_checksum() {
  local name="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$DIST_DIR" && sha256sum "$name" > "$name.sha256")
  elif command -v shasum >/dev/null 2>&1; then
    (cd "$DIST_DIR" && shasum -a 256 "$name" > "$name.sha256")
  fi
}

build_one() {
  local target="$1"
  local name="$2"
  echo "==> building $name ($target)"
  ensure_target "$target"
  (
    cd "$KLEPTO_DIR"
    unset CARGO_TARGET_DIR
    export CARGO_TARGET_DIR="$KLEPTO_DIR/target"
    cargo build --release --target "$target"
  )
  mkdir -p "$DIST_DIR"
  local src="$KLEPTO_DIR/target/$target/release/klepto"
  if [[ ! -f "$src" ]]; then
    echo "error: built binary not found for $target" >&2
    find "$KLEPTO_DIR/target" -name klepto -type f 2>/dev/null | head -20 >&2 || true
    exit 1
  fi
  local out="$DIST_DIR/klepto-$name"
  cp "$src" "$out"
  chmod +x "$out"
  codesign --force --sign - "$out"
  write_checksum "klepto-$name"
  if command -v file >/dev/null 2>&1; then
    file "$out"
  fi
  ls -lh "$out"
}

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: darwin builds require a macOS host (Apple SDK)" >&2
  exit 1
fi

need_cmd cargo
need_cmd rustup
need_cmd codesign

case "$MODE" in
  amd64|x86_64|darwin-amd64|macos-amd64|macos-x86_64)
    build_one x86_64-apple-darwin darwin-amd64
    ;;
  arm64|aarch64|darwin-arm64|macos-arm64)
    build_one aarch64-apple-darwin darwin-arm64
    ;;
  all)
    build_one aarch64-apple-darwin darwin-arm64
    build_one x86_64-apple-darwin darwin-amd64
    ;;
  *)
    echo "usage: $0 [amd64|arm64|all]" >&2
    exit 1
    ;;
esac

echo "Artifacts in $DIST_DIR:"
ls -lh "$DIST_DIR"/klepto-darwin-* 2>/dev/null || true
