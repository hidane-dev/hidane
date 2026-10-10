#!/bin/sh
# Installs hidane, a Firestore emulator without Java, from its GitHub Releases:
#
#   curl -fsSL https://hidane.dev/install.sh | sh
#
# macOS (arm64, x86_64) and Linux (arm64, x86_64; static, any distribution). The archive is
# checked against the release's sha256sums.txt before anything is installed.
#
#   HIDANE_VERSION       a version to install, such as 0.1.0 (default: the latest release)
#   HIDANE_INSTALL_DIR   where to put the binary (default: ~/.local/bin)
#   HIDANE_RELEASES_URL  a mirror of the release files, holding v<version>/<file>
set -eu

repo="https://github.com/hidane-dev/hidane"
releases="${HIDANE_RELEASES_URL:-$repo/releases/download}"
dir="${HIDANE_INSTALL_DIR:-$HOME/.local/bin}"

fail() {
  echo "hidane: $*" >&2
  exit 1
}

if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL "$1" -o "$2"; }
  # The latest release page redirects to .../releases/tag/v<version>.
  latest() { curl -fsSLI -o /dev/null -w '%{url_effective}' "$repo/releases/latest" | sed -n 's|.*/releases/tag/v||p'; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -q "$1" -O "$2"; }
  latest() { wget -qO- https://api.github.com/repos/hidane-dev/hidane/releases/latest | sed -n 's/.*"tag_name": *"v\([^"]*\)".*/\1/p'; }
else
  fail "curl or wget is needed"
fi

os=$(uname -s)
arch=$(uname -m)
# A shell under Rosetta reports x86_64 on Apple silicon; the arm64 binary is the native one.
if [ "$os" = Darwin ] && [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
  arch=arm64
fi
case "$os $arch" in
  "Darwin arm64") target=aarch64-apple-darwin ;;
  "Darwin x86_64") target=x86_64-apple-darwin ;;
  "Linux aarch64" | "Linux arm64") target=aarch64-unknown-linux-musl ;;
  "Linux x86_64" | "Linux amd64") target=x86_64-unknown-linux-musl ;;
  *) fail "no prebuilt binary for $os $arch; npm (npm i -D hidane) or cargo (cargo install hidane) may work" ;;
esac

version="${HIDANE_VERSION:-}"
if [ -z "$version" ]; then
  version=$(latest 2>/dev/null || true)
  [ -n "$version" ] || fail "found no release of hidane; see $repo/releases"
fi
version="${version#v}"

archive="hidane-$version-$target.tar.gz"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

echo "hidane: downloading $archive"
fetch "$releases/v$version/$archive" "$tmp/$archive" || fail "could not download $releases/v$version/$archive"
fetch "$releases/v$version/sha256sums.txt" "$tmp/sha256sums.txt" || fail "could not download sha256sums.txt"

expected=$(awk -v f="$archive" '$2 == f || $2 == "*" f { print $1 }' "$tmp/sha256sums.txt")
[ -n "$expected" ] || fail "$archive is not listed in sha256sums.txt"
if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$tmp/$archive" | awk '{ print $1 }')
else
  actual=$(shasum -a 256 "$tmp/$archive" | awk '{ print $1 }')
fi
[ "$actual" = "$expected" ] || fail "$archive has SHA-256 $actual, sha256sums.txt says $expected"

tar -xzf "$tmp/$archive" -C "$tmp"
mkdir -p "$dir"
mv "$tmp/hidane-$version-$target/hidane" "$dir/hidane"
chmod 755 "$dir/hidane"
echo "hidane: installed $("$dir/hidane" --version) in $dir"

case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "hidane: $dir is not on your PATH; add it, for example: export PATH=\"$dir:\$PATH\"" ;;
esac
