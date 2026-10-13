#!/usr/bin/env bash
# Builds the hidane binary of a release for one target, as the release workflow does, so that a
# commit gives the same bytes wherever it is built (docs/reproducible-builds.md):
#
#   packaging/release/build.sh <target> [<THIRD-PARTY-LICENSES.txt>]
#
# - Rust as pinned in packaging/release/rust-toolchain.toml, installed with rustup if missing;
# - `--locked`, so Cargo.lock decides every dependency, and the target directory in the checkout;
# - the checkout and the Cargo home written as /hidane and /cargo in the paths the binary keeps
#   (panic locations), so that neither where the repository is checked out nor whose home it is
#   ends up in the binary;
# - on Windows, a linker that writes no time stamp and no PDB path into the executable;
# - the third-party notices embedded from the given file (`hidane --licenses`). The release writes
#   it with cargo-about; to rebuild a release, take it from the release's archive.
#
# The binary is target/<target>/release/hidane (hidane.exe on Windows). RUSTFLAGS and other
# compiler flags from the environment are replaced; a profile or linker set in a Cargo
# configuration of your own (~/.cargo/config.toml) still applies, and changes the result.
set -euo pipefail

target=${1:?usage: build.sh <target> [<THIRD-PARTY-LICENSES.txt>]}
notices=${2:-}
root=$(cd "$(dirname "$0")/../.." && pwd)
toolchain=$(sed -n 's/^channel = "\(.*\)"$/\1/p' "$root/packaging/release/rust-toolchain.toml")
if [[ -z "$toolchain" ]]; then
  echo "build.sh: no channel in packaging/release/rust-toolchain.toml" >&2
  exit 1
fi

# rustc sees native paths: C:\a\hidane rather than /c/a/hidane under Git Bash on Windows.
native() {
  if command -v cygpath > /dev/null; then cygpath -w "$1"; else printf '%s\n' "$1"; fi
}

flags=(
  "--remap-path-prefix=$(native "$root")=/hidane"
  "--remap-path-prefix=$(native "${CARGO_HOME:-$HOME/.cargo}")=/cargo"
)
if [[ "$target" == *-windows-msvc ]]; then
  flags+=("-Clink-arg=/Brepro" "-Clink-arg=/PDBALTPATH:%_PDB%")
fi
# Cargo's 0x1f-separated form keeps paths with spaces whole, and takes precedence over RUSTFLAGS
# and the `rustflags` of any Cargo configuration.
CARGO_ENCODED_RUSTFLAGS=$(
  IFS=$'\x1f'
  printf '%s' "${flags[*]}"
)
export CARGO_ENCODED_RUSTFLAGS
export RUSTUP_TOOLCHAIN="$toolchain"
unset RUSTFLAGS CARGO_BUILD_RUSTFLAGS CARGO_TARGET_DIR CARGO_BUILD_TARGET_DIR

if [[ -n "$notices" ]]; then
  HIDANE_THIRD_PARTY_LICENSES=$(native "$(cd "$(dirname "$notices")" && pwd)/$(basename "$notices")")
  export HIDANE_THIRD_PARTY_LICENSES
else
  unset HIDANE_THIRD_PARTY_LICENSES
fi

rustup toolchain install "$toolchain" --profile minimal --target "$target" --no-self-update
cd "$root"
rustc --version --verbose
cargo build --release --locked -p hidane --target "$target" --target-dir target
