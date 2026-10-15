#!/bin/sh
# Installs the Firestore package of a google-cloud-node checkout (handwritten/firestore, with the
# repository's bin/ for its test runner) with its own package manager (pnpm 10, Node.js 22 or
# later), and compiles it and its system tests to build/.
#
#   tools/nodejs-system-tests/setup.sh <google-cloud-node checkout>
#
# A sparse checkout of `handwritten/firestore` and `bin` is enough (README.md). Install scripts
# do not run: the package needs none to compile.
set -eu

cd "$1"
pnpm install --filter @google-cloud/firestore --ignore-scripts
cd handwritten/firestore
pnpm run compile
