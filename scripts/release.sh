#!/usr/bin/env bash
# Create and push a v<version> tag so CI builds artifacts and attaches them
# to the GitHub Release. Version comes from klepto/Cargo.toml.
#
# Usage: ./scripts/release.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: '$1' is required" >&2
    exit 1
  }
}

need_cmd git

if [[ -n "$(git status --porcelain)" ]]; then
  echo "error: working tree is dirty — commit or stash before releasing" >&2
  git status --short >&2
  exit 1
fi

version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' klepto/Cargo.toml | head -1)"
if [[ -z "$version" || ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "error: could not parse semver from klepto/Cargo.toml (got '$version')" >&2
  exit 1
fi

ext_version="$(sed -n 's/^  "version": "\([^"]*\)".*/\1/p' klepto-vscode/package.json | head -1)"
if [[ "$ext_version" != "$version" ]]; then
  echo "error: klepto-vscode version $ext_version does not match klepto $version" >&2
  echo "Run: make bump" >&2
  exit 1
fi

tag="v${version}"

if git rev-parse "$tag" >/dev/null 2>&1; then
  echo "error: tag $tag already exists locally" >&2
  echo "Bump first: make bump && git add -u && git commit -m \"Bump version to $version\"" >&2
  exit 1
fi

if git ls-remote --exit-code --tags origin "refs/tags/$tag" >/dev/null 2>&1; then
  echo "error: tag $tag already exists on origin" >&2
  exit 1
fi

git tag -a "$tag" -m "$tag"
git push origin "$tag"
echo "Pushed $tag. CI will build binaries and the VSIX, then attach them to the GitHub Release."
