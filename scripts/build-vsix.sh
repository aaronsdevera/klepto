#!/usr/bin/env bash
# Package the Klepto VS Code / VSCodium extension.
# Usage: ./scripts/build-vsix.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
EXT_DIR="$ROOT/klepto-vscode"
DIST_DIR="$ROOT/dist"

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: '$1' is required" >&2
    exit 1
  }
}

write_checksum() {
  local name="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$DIST_DIR" && sha256sum "$name" > "$name.sha256")
  elif command -v shasum >/dev/null 2>&1; then
    (cd "$DIST_DIR" && shasum -a 256 "$name" > "$name.sha256")
  fi
}

need_cmd npm
mkdir -p "$DIST_DIR"

(
  cd "$EXT_DIR"
  if [[ -f package-lock.json ]]; then
    npm ci
  else
    npm install
  fi
  npm run package
)

vsix="$(ls -1t "$EXT_DIR"/*.vsix 2>/dev/null | head -1 || true)"
if [[ -z "$vsix" ]]; then
  echo "error: no .vsix produced" >&2
  exit 1
fi

name="$(basename "$vsix")"
cp "$vsix" "$DIST_DIR/$name"
write_checksum "$name"
ls -lh "$DIST_DIR/$name"

echo "Artifacts in $DIST_DIR:"
ls -lh "$DIST_DIR"/"$name"* 2>/dev/null || true
