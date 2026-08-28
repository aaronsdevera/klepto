#!/usr/bin/env sh
# Install released Klepto daemon + VS Code / VSCodium extension.
#
# Non-interactive one-liner (recommended):
#   curl -fsSL https://raw.githubusercontent.com/aaronsdevera/klepto/main/scripts/install.sh | sh -s -- -y
#
# Env (same meaning as flags):
#   KLEPTO_REPO, KLEPTO_VERSION, KLEPTO_INSTALL_DIR
#   KLEPTO_SKIP_EXTENSION=1, KLEPTO_SKIP_DOCTOR=1, KLEPTO_YES=1
#   KLEPTO_EDITOR=/path/to/code
set -eu

REPO="${KLEPTO_REPO:-aaronsdevera/klepto}"
VERSION="${KLEPTO_VERSION:-latest}"
DEST="${KLEPTO_INSTALL_DIR:-$HOME/.klepto/bin}"
SKIP_EXTENSION="${KLEPTO_SKIP_EXTENSION:-0}"
SKIP_DOCTOR="${KLEPTO_SKIP_DOCTOR:-0}"
YES="${KLEPTO_YES:-0}"
EDITOR_BIN="${KLEPTO_EDITOR:-}"
REQUIRE_EDITOR=0

usage() {
  cat <<'EOF'
Usage: install.sh [options]

Non-interactive install (safe for curl | sh):
  curl -fsSL .../scripts/install.sh | sh -s -- -y

Options:
  -y, --yes, --non-interactive
                          Never prompt; close stdin for child commands;
                          set CI/NONINTERACTIVE package-manager env
  --version <tag>         Release tag (e.g. v0.5.3 or 0.5.3). Default: latest
  --dir <path>            Daemon install directory (default: ~/.klepto/bin)
  --editor <path>         Editor CLI for VSIX install (code/codium/cursor)
  --skip-extension        Install the daemon only
  --skip-doctor           Do not run klepto doctor --install
  --require-editor        With extension install, fail if no editor CLI
  --configure-path [dir]  Only add dir to PATH, then exit
  -h, --help              Show this help

Environment: KLEPTO_REPO, KLEPTO_VERSION, KLEPTO_INSTALL_DIR, KLEPTO_EDITOR,
             KLEPTO_SKIP_EXTENSION, KLEPTO_SKIP_DOCTOR, KLEPTO_YES
EOF
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: '$1' is required" >&2
    exit 1
  }
}

# Run a command with stdin detached so a piped installer cannot hang on prompts.
run_ni() {
  if [ "$YES" = 1 ] || [ ! -t 0 ]; then
    "$@" </dev/null
  else
    "$@"
  fi
}

enable_noninteractive_env() {
  export CI="${CI:-1}"
  export NONINTERACTIVE=1
  export DEBIAN_FRONTEND=noninteractive
  export HOMEBREW_NO_AUTO_UPDATE=1
  export HOMEBREW_NO_ANALYTICS=1
  export HOMEBREW_NO_ENV_HINTS=1
  # Prefer failing over a sudo password prompt when -y is set.
  if [ "$YES" = 1 ]; then
    export SUDO_NONINTERACTIVE=1
  fi
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  else
    echo "error: sha256sum or shasum is required" >&2
    exit 1
  fi
}

verify_download() {
  asset_name="$1"
  file_path="$2"
  checksum_path="$3"

  expected="$(awk 'NR == 1 { print $1 }' "$checksum_path" | tr '[:upper:]' '[:lower:]')"
  case "$expected" in
    *[!0-9a-fA-F]* | '')
      echo "error: invalid checksum sidecar for $asset_name" >&2
      exit 1
      ;;
  esac
  [ "${#expected}" -eq 64 ] || {
    echo "error: invalid checksum sidecar for $asset_name" >&2
    exit 1
  }

  actual="$(sha256_file "$file_path" | tr '[:upper:]' '[:lower:]')"
  [ "$expected" = "$actual" ] || {
    echo "error: checksum mismatch for $asset_name" >&2
    exit 1
  }
}

configure_path() {
  dest="$1"
  shell_path="${SHELL:-}"
  shell_name="${shell_path##*/}"

  case "$shell_name" in
    fish)
      if command -v fish >/dev/null 2>&1; then
        run_ni env KLEPTO_INSTALL_PATH="$dest" fish -c 'fish_add_path --move "$KLEPTO_INSTALL_PATH"'
        echo "Added $dest to the fish PATH."
      else
        echo "Add $dest to your fish PATH with: fish_add_path \"$dest\"" >&2
      fi
      ;;
    zsh | bash)
      if [ "$shell_name" = zsh ]; then
        profile="$HOME/.zshrc"
      else
        profile="$HOME/.bashrc"
      fi
      path_line="export PATH=\"$dest:\$PATH\""
      touch "$profile"
      if ! grep -Fqx "$path_line" "$profile"; then
        printf '\n%s\n' "$path_line" >>"$profile"
        echo "Added $dest to PATH in $profile."
      fi
      ;;
    *)
      case ":$PATH:" in
        *":$dest:"*) ;;
        *) echo "Add this to your shell profile: export PATH=\"$dest:\$PATH\"" >&2 ;;
      esac
      ;;
  esac
}

resolve_tag() {
  if [ "$VERSION" != latest ]; then
    case "$VERSION" in
      v*) echo "$VERSION" ;;
      *) echo "v$VERSION" ;;
    esac
    return
  fi

  # Follow /releases/latest -> .../tag/vX.Y.Z (no GitHub API token required).
  need_cmd curl
  url="$(run_ni curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest")"
  tag="${url##*/}"
  case "$tag" in
    v[0-9]*) echo "$tag" ;;
    *)
      echo "error: could not resolve latest release tag from $url" >&2
      exit 1
      ;;
  esac
}

download_verified() {
  asset_name="$1"
  out_path="$2"
  base="$3"

  tmp="$(mktemp "${TMPDIR:-/tmp}/klepto.download.XXXXXX")"
  checksum="$tmp.sha256"
  cleanup_dl() {
    rm -f "$tmp" "$checksum"
  }
  trap cleanup_dl EXIT HUP INT TERM

  echo "Downloading $asset_name..."
  run_ni curl -fsSL "$base/$asset_name" -o "$tmp"
  run_ni curl -fsSL "$base/$asset_name.sha256" -o "$checksum"
  verify_download "$asset_name" "$tmp" "$checksum"
  mv -f "$tmp" "$out_path"
  trap - EXIT HUP INT TERM
  cleanup_dl
}

resolve_editor() {
  if [ -n "$EDITOR_BIN" ]; then
    if [ -x "$EDITOR_BIN" ] || command -v "$EDITOR_BIN" >/dev/null 2>&1; then
      echo "$EDITOR_BIN"
      return 0
    fi
    echo "error: --editor '$EDITOR_BIN' not found or not executable" >&2
    exit 1
  fi

  for cmd in code codium code-insiders cursor; do
    if command -v "$cmd" >/dev/null 2>&1; then
      command -v "$cmd"
      return 0
    fi
  done

  if [ "$(uname -s)" = Darwin ]; then
    for path in \
      "/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code" \
      "/Applications/VSCodium.app/Contents/Resources/app/bin/codium" \
      "/Applications/Cursor.app/Contents/Resources/app/bin/cursor"; do
      if [ -x "$path" ]; then
        echo "$path"
        return 0
      fi
    done
  fi
  return 1
}

install_extension() {
  tag="$1"
  base="$2"
  ver="${tag#v}"
  vsix_name="klepto-vscode-${ver}.vsix"
  vsix_dir="${XDG_CACHE_HOME:-$HOME/.cache}/klepto"
  mkdir -p "$vsix_dir"
  vsix_path="$vsix_dir/$vsix_name"

  download_verified "$vsix_name" "$vsix_path" "$base"
  echo "Downloaded extension to $vsix_path"

  if editor="$(resolve_editor)"; then
    echo "Installing extension with $editor..."
    if run_ni "$editor" --install-extension "$vsix_path" --force; then
      echo "Installed Klepto extension via $editor."
      echo "Reload the editor window if it is already open."
      return 0
    fi
    echo "error: $editor --install-extension failed" >&2
    echo "VSIX left at $vsix_path" >&2
    exit 1
  fi

  if [ "$REQUIRE_EDITOR" = 1 ]; then
    echo "error: no VS Code / VSCodium / Cursor CLI found (required by --require-editor)" >&2
    echo "VSIX left at $vsix_path" >&2
    exit 1
  fi

  echo "No editor CLI found; VSIX saved to $vsix_path"
  echo "Install later with: code --install-extension $vsix_path"
}

# --- args ---
while [ "$#" -gt 0 ]; do
  case "$1" in
    -y | --yes | --non-interactive)
      YES=1
      shift
      ;;
    --version)
      [ "$#" -ge 2 ] || {
        echo "error: --version requires a tag" >&2
        exit 1
      }
      VERSION="$2"
      shift 2
      ;;
    --version=*)
      VERSION="${1#--version=}"
      shift
      ;;
    --dir | --install-dir)
      [ "$#" -ge 2 ] || {
        echo "error: $1 requires a path" >&2
        exit 1
      }
      DEST="$2"
      shift 2
      ;;
    --dir=* | --install-dir=*)
      DEST="${1#*=}"
      shift
      ;;
    --editor)
      [ "$#" -ge 2 ] || {
        echo "error: --editor requires a path or command" >&2
        exit 1
      }
      EDITOR_BIN="$2"
      shift 2
      ;;
    --editor=*)
      EDITOR_BIN="${1#--editor=}"
      shift
      ;;
    --configure-path)
      if [ "$#" -ge 2 ] && [ "${2#-}" = "$2" ]; then
        configure_path "$2"
      else
        configure_path "$DEST"
      fi
      exit 0
      ;;
    --configure-path=*)
      configure_path "${1#--configure-path=}"
      exit 0
      ;;
    --skip-extension)
      SKIP_EXTENSION=1
      shift
      ;;
    --skip-doctor)
      SKIP_DOCTOR=1
      shift
      ;;
    --require-editor)
      REQUIRE_EDITOR=1
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown option: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

# Piped installs are non-interactive even without -y (stdin is the script body).
if [ ! -t 0 ]; then
  YES=1
fi
if [ "$YES" = 1 ]; then
  enable_noninteractive_env
fi

need_cmd curl
need_cmd uname
need_cmd mktemp

os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"
case "$os:$arch" in
  darwin:arm64 | darwin:aarch64) asset="klepto-darwin-arm64" ;;
  darwin:x86_64) asset="klepto-darwin-amd64" ;;
  linux:x86_64 | linux:amd64) asset="klepto-linux-amd64" ;;
  linux:arm64 | linux:aarch64) asset="klepto-linux-arm64" ;;
  *)
    echo "error: unsupported platform: $os/${arch}" >&2
    echo "Precompiled install supports macOS and Linux. See https://github.com/$REPO/releases" >&2
    exit 1
    ;;
esac

tag="$(resolve_tag)"
base="https://github.com/$REPO/releases/download/$tag"
echo "Installing Klepto $tag for ${os}/${arch}..."

mkdir -p "$DEST"
DEST="$(cd "$DEST" && pwd -P)"
bin_path="$DEST/klepto"
download_verified "$asset" "$bin_path" "$base"
chmod 755 "$bin_path"
if [ "$os" = darwin ]; then
  codesign --force --sign - "$bin_path" >/dev/null 2>&1 || true
fi
configure_path "$DEST"
echo "Installed daemon to $bin_path"

if [ "$SKIP_EXTENSION" != 1 ]; then
  install_extension "$tag" "$base"
fi

if [ "$SKIP_DOCTOR" != 1 ]; then
  echo "Checking runtime dependencies..."
  if ! run_ni "$bin_path" doctor --install; then
    echo "error: klepto doctor --install failed" >&2
    echo "Re-run with --skip-doctor to install the binary only, then fix deps manually." >&2
    exit 1
  fi
fi

echo
echo "Klepto $tag is ready."
echo "Open a project in VS Code or VSCodium, then press Cmd+L / Ctrl+L to chat."
