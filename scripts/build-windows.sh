#!/usr/bin/env bash
# Build Klepto release binaries for Windows (MSVC).
# Requires a Windows host with the MSVC toolchain.
#
# Usage: ./scripts/build-windows.sh [amd64]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
KLEPTO_DIR="$ROOT/klepto"
DIST_DIR="$ROOT/dist"
MODE="${1:-amd64}"

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
  elif command -v certutil >/dev/null 2>&1; then
    local hash
    hash="$(certutil -hashfile "$DIST_DIR/$name" SHA256 | awk 'NR==2 { print $1 }' | tr '[:upper:]' '[:lower:]')"
    printf '%s  %s\n' "$hash" "$name" > "$DIST_DIR/$name.sha256"
  else
    echo "error: sha256sum, shasum, or certutil is required" >&2
    exit 1
  fi
}

is_windows() {
  case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*|Windows_NT) return 0 ;;
    *) return 1 ;;
  esac
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
  local src="$KLEPTO_DIR/target/$target/release/klepto.exe"
  if [[ ! -f "$src" ]]; then
    echo "error: built binary not found for $target" >&2
    find "$KLEPTO_DIR/target" -name 'klepto.exe' -type f 2>/dev/null | head -20 >&2 || true
    exit 1
  fi
  local out="$DIST_DIR/klepto-$name.exe"
  cp "$src" "$out"
  write_checksum "klepto-$name.exe"
  ls -lh "$out"
}

if ! is_windows; then
  echo "error: windows builds require a Windows host (MSVC)" >&2
  exit 1
fi

need_cmd cargo
need_cmd rustup

case "$MODE" in
  amd64|x86_64|windows-amd64)
    build_one x86_64-pc-windows-msvc windows-amd64
    ;;
  *)
    echo "usage: $0 [amd64]" >&2
    exit 1
    ;;
esac

echo "Artifacts in $DIST_DIR:"
ls -lh "$DIST_DIR"/klepto-windows-* 2>/dev/null || true
