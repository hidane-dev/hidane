#!/usr/bin/env bash
# Downloads the official Cloud Firestore emulator jar from URL to JAR unless JAR is already there
# (restored from the cache), then checks its SHA-256 against SHA256 either way. A jar that does
# not match is deleted, so that it is never cached or run.
#
# Environment: URL, JAR, SHA256.
set -euo pipefail

if [[ ! -f "$JAR" ]]; then
  mkdir -p "$(dirname "$JAR")"
  echo "Downloading $URL"
  curl --fail --silent --show-error --location --retry 5 --retry-all-errors \
    --connect-timeout 30 --output "$JAR.part" "$URL"
  mv "$JAR.part" "$JAR"
fi

if command -v sha256sum > /dev/null; then
  actual=$(sha256sum "$JAR" | cut -d ' ' -f 1)
else
  actual=$(shasum -a 256 "$JAR" | cut -d ' ' -f 1)
fi
if [[ "$actual" != "$SHA256" ]]; then
  rm -f "$JAR"
  echo "::error title=Official emulator::The jar from $URL has SHA-256 $actual, not $SHA256"
  exit 1
fi
echo "$JAR: SHA-256 $actual, as expected"
