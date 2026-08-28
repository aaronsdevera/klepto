#!/usr/bin/env bash
# Bump klepto (Rust) + klepto-vscode versions in lockstep.
#
# Usage:
#   ./scripts/bump-version.sh          # fix (patch +1)
#   ./scripts/bump-version.sh fix
#   ./scripts/bump-version.sh minor
#   ./scripts/bump-version.sh major
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
KIND="${1:-fix}"

case "$KIND" in
  major|minor|fix) ;;
  patch) KIND=fix ;;
  *)
    echo "error: unknown bump kind '$KIND' (expected major, minor, or fix)" >&2
    exit 1
    ;;
esac

CARGO_TOML="$ROOT/klepto/Cargo.toml"
CARGO_LOCK="$ROOT/klepto/Cargo.lock"
PKG_JSON="$ROOT/klepto-vscode/package.json"
PKG_LOCK="$ROOT/klepto-vscode/package-lock.json"
FLAKE_NIX="$ROOT/flake.nix"

current="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$CARGO_TOML" | head -1)"
if [[ -z "$current" || ! "$current" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "error: could not parse semver from $CARGO_TOML (got '$current')" >&2
  exit 1
fi

IFS=. read -r major minor patch <<<"$current"
case "$KIND" in
  major) major=$((major + 1)); minor=0; patch=0 ;;
  minor) minor=$((minor + 1)); patch=0 ;;
  fix)   patch=$((patch + 1)) ;;
esac
next="${major}.${minor}.${patch}"

echo "Bumping $KIND: $current → $next"

python3 - "$current" "$next" "$CARGO_TOML" "$CARGO_LOCK" "$PKG_JSON" "$PKG_LOCK" "$FLAKE_NIX" <<'PY'
import re
import sys

cur, nxt = sys.argv[1], sys.argv[2]
cargo_toml, cargo_lock, pkg_json, pkg_lock, flake = sys.argv[3:8]


def write(path: str, text: str) -> None:
    with open(path, "w", encoding="utf-8") as f:
        f.write(text)


def must_sub(path: str, pattern: str, repl: str, count: int = 1, flags: int = 0) -> None:
    with open(path, encoding="utf-8") as f:
        text = f.read()
    new, n = re.subn(pattern, repl, text, count=count, flags=flags)
    if n < 1:
        raise SystemExit(f"error: pattern not found in {path}: {pattern!r}")
    write(path, new)


# Cargo.toml package version (first ^version = "…" line)
must_sub(
    cargo_toml,
    rf'(?m)^(version = )"{re.escape(cur)}"',
    rf'\1"{nxt}"',
)

# flake.nix klepto package version
must_sub(
    flake,
    rf'(version = )"{re.escape(cur)}";',
    rf'\1"{nxt}";',
)

# package.json top-level version
must_sub(
    pkg_json,
    rf'("version": )"{re.escape(cur)}"',
    rf'\1"{nxt}"',
)

# package-lock.json: root version + packages[""].version only
with open(pkg_lock, encoding="utf-8") as f:
    lock = f.read()

lock2, n1 = re.subn(
    rf'^(\{{\s*"name": "klepto-vscode",\s*"version": )"{re.escape(cur)}"',
    rf'\1"{nxt}"',
    lock,
    count=1,
    flags=re.M,
)
if n1 != 1:
    raise SystemExit(f"error: root version not found in {pkg_lock}")

lock3, n2 = re.subn(
    rf'("packages": \{{\s*"": \{{\s*"name": "klepto-vscode",\s*"version": )"{re.escape(cur)}"',
    rf'\1"{nxt}"',
    lock2,
    count=1,
    flags=re.M,
)
if n2 != 1:
    raise SystemExit(f"error: packages[\"\"] version not found in {pkg_lock}")
write(pkg_lock, lock3)

# Cargo.lock: only [[package]] name = "klepto"
with open(cargo_lock, encoding="utf-8") as f:
    lines = f.readlines()

out = []
in_klepto = False
replaced = False
for line in lines:
    if line.startswith("[[package]]"):
        in_klepto = False
    if line == 'name = "klepto"\n':
        in_klepto = True
    if in_klepto and line == f'version = "{cur}"\n':
        out.append(f'version = "{nxt}"\n')
        replaced = True
        in_klepto = False
        continue
    out.append(line)

if not replaced:
    raise SystemExit(f"error: klepto package version not found in {cargo_lock}")
write(cargo_lock, "".join(out))
PY

echo "Updated:"
echo "  $CARGO_TOML"
echo "  $CARGO_LOCK"
echo "  $PKG_JSON"
echo "  $PKG_LOCK"
echo "  $FLAKE_NIX"
echo "version $next"
