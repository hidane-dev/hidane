#!/bin/sh
# Stages a release's npm packages, built by assemble.mjs into <dir>, for a maintainer to approve
# on npmjs.com with 2FA: the platform packages first, then hidane, which depends on them.
#
#   sh packaging/npm/stage.sh <version> <dir> [npm flags, such as --provenance]
#
# Needs npm 11.15.0 or newer. A version already published is skipped; one already staged (when a
# run is repeated) is reported and left for approval.
set -eu

version=$1
dir=$(cd "$2" && pwd)
shift 2

npm_version=$(npm --version)
if ! node -e 'const [a, b] = process.argv[1].split(".").map(Number); process.exit(a > 11 || (a === 11 && b >= 15) ? 0 : 1)' "$npm_version"; then
  echo "npm $npm_version cannot stage packages; npm 11.15.0 or newer is needed" >&2
  exit 1
fi

failed=0
for package in darwin-arm64 darwin-x64 linux-arm64 linux-x64 win32-x64 hidane; do
  name=$(node -p "require('$dir/$package/package.json').name")
  if [ "$package" = hidane ] && [ "$failed" = 1 ]; then
    echo "not staging $name@$version: a platform package failed, and hidane depends on all of them" >&2
    break
  fi
  if [ "$(npm view "$name@$version" version 2>/dev/null)" = "$version" ]; then
    echo "$name@$version is already published"
    continue
  fi
  if out=$(cd "$dir/$package" && npm stage publish --access public "$@" 2>&1); then
    echo "staged $name@$version"
  elif printf '%s' "$out" | grep -qiE 'already|E409|409 Conflict'; then
    echo "$name@$version is already staged"
  else
    printf '%s\n' "$out" >&2
    echo "could not stage $name@$version" >&2
    failed=1
  fi
done
exit "$failed"
