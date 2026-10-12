#!/bin/sh
# Installs a firebase-js-sdk checkout with its own package manager (Yarn 1, Node.js 24) and builds
# the packages the Firestore browser tests import from their dist/ (the tests compile Firestore
# from its sources). Playwright's Chromium is installed separately (README.md).
#
#   tools/js-sdk-browser/setup.sh <firebase-js-sdk checkout>
set -eu

cd "$1"
PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 yarn install --frozen-lockfile --prefer-offline
npx lerna run build --include-dependencies \
  --scope @firebase/util --scope @firebase/logger --scope @firebase/component \
  --scope @firebase/app --scope @firebase/webchannel-wrapper --scope @firebase/auth
