#!/usr/bin/env bash
# Build Klepto release binaries for Debian-compatible Linux (glibc).
# Uses cargo-zigbuild (no Docker). Works on macOS and Linux hosts.
#
# Usage: ./scripts/build-linux.sh [amd64|arm64|all]
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

ensure_zig() {
  if command -v zig >/dev/null 2>&1; then
    return 0
  fi
  if [[ -x "$ROOT/.oci/zig" ]]; then
    export PATH="$ROOT/.oci:$PATH"
    if command -v zig >/dev/null 2>&1; then
      echo "==> using zig $(zig version 2>/dev/null || true)"
      return 0
    fi
  fi

  need_cmd python3
  mkdir -p "$ROOT/.oci"

  # Project-local venv avoids PEP 668 (Homebrew Python blocks system pip --user).
  local venv="$ROOT/.oci/zig-venv"
  echo "zig not found - installing ziglang into ${venv}..."
  if [[ ! -x "${venv}/bin/python" ]]; then
    python3 -m venv "${venv}"
  fi
  "${venv}/bin/pip" install -q --upgrade pip
  "${venv}/bin/pip" install -q ziglang

  local zig_bin
  zig_bin="$("${venv}/bin/python" - <<'PY'
import pathlib
import ziglang
print(pathlib.Path(ziglang.__file__).resolve().parent / "zig")
PY
)"
  if [[ -x "$zig_bin" ]]; then
    ln -sfn "$zig_bin" "$ROOT/.oci/zig"
  else
    cat >"$ROOT/.oci/zig" <<EOF
#!/usr/bin/env bash
exec "${venv}/bin/python" -m ziglang "\$@"
EOF
    chmod +x "$ROOT/.oci/zig"
  fi
  export PATH="$ROOT/.oci:$PATH"

  if ! command -v zig >/dev/null 2>&1 && command -v brew >/dev/null 2>&1; then
    echo "venv zig wrapper failed - trying Homebrew..."
    brew install zig
  fi

  command -v zig >/dev/null 2>&1 || {
    echo "error: zig still not on PATH after install attempt" >&2
    echo "Install manually: brew install zig" >&2
    echo "  or: python3 -m venv .oci/zig-venv && .oci/zig-venv/bin/pip install ziglang" >&2
    exit 1
  }
  echo "==> using zig $(zig version 2>/dev/null || true)"
}

ensure_zigbuild() {
  if cargo zigbuild --help >/dev/null 2>&1; then
    return 0
  fi
  echo "cargo-zigbuild not found — installing…"
  need_cmd cargo
  cargo install cargo-zigbuild --locked
}

ensure_target() {
  local target="$1"
  rustup target add "$target" >/dev/null
}

build_one() {
  local target="$1"
  local name="$2"
  local zig_target="${3:-$target}"
  echo "==> building $name ($zig_target) with cargo zigbuild"
  ensure_target "$target"
  (
    cd "$KLEPTO_DIR"
    # Avoid Cursor/sandbox CARGO_TARGET_DIR overrides filling tmp volumes.
    unset CARGO_TARGET_DIR
    export CARGO_TARGET_DIR="$KLEPTO_DIR/target"
    cargo zigbuild --release --target "$zig_target"
  )
  mkdir -p "$DIST_DIR"
  local src="$KLEPTO_DIR/target/$target/release/klepto"
  # zigbuild may place output under target/<triple>.2.28/ or target/<triple>/
  if [[ ! -f "$src" && "$zig_target" != "$target" ]]; then
    src="$KLEPTO_DIR/target/${zig_target}/release/klepto"
  fi
  if [[ ! -f "$src" ]]; then
    echo "error: built binary not found for $target" >&2
    find "$KLEPTO_DIR/target" -name klepto -type f 2>/dev/null | head -20 >&2 || true
    exit 1
  fi
  local out="$DIST_DIR/klepto-$name"
  cp "$src" "$out"
  chmod +x "$out"
  if [[ "$name" == darwin-* ]]; then
    codesign --force --sign - "$out"
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$DIST_DIR" && sha256sum "klepto-$name" > "klepto-$name.sha256")
  elif command -v shasum >/dev/null 2>&1; then
    (cd "$DIST_DIR" && shasum -a 256 "klepto-$name" > "klepto-$name.sha256")
  fi
  if command -v file >/dev/null 2>&1; then
    file "$out"
  fi
  ls -lh "$out"
}

need_cmd cargo
need_cmd rustup
ensure_zig
ensure_zigbuild

case "$MODE" in
  amd64|x86_64|linux-amd64)
    build_one x86_64-unknown-linux-gnu linux-amd64 x86_64-unknown-linux-gnu.2.28
    ;;
  arm64|aarch64|arm|linux-arm64)
    build_one aarch64-unknown-linux-gnu linux-arm64 aarch64-unknown-linux-gnu.2.28
    ;;
  darwin-amd64|macos-amd64|macos-x86_64)
    exec "$ROOT/scripts/build-darwin.sh" amd64
    ;;
  all)
    build_one x86_64-unknown-linux-gnu linux-amd64 x86_64-unknown-linux-gnu.2.28
    build_one aarch64-unknown-linux-gnu linux-arm64 aarch64-unknown-linux-gnu.2.28
    ;;
  *)
    echo "usage: $0 [amd64|arm64|darwin-amd64|all]" >&2
    echo "note: darwin-amd64 is a compatibility alias for scripts/build-darwin.sh" >&2
    exit 1
    ;;
esac

echo "Artifacts in $DIST_DIR:"
ls -lh "$DIST_DIR"/klepto-* 2>/dev/null || true
