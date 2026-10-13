# Reproducible builds

A release's binaries and archives can be rebuilt from its tag, and the rebuild is the same bytes
as what was published. Checksums and build provenance ([releasing.md](releasing.md#assets)) say
that an archive is the one GitHub Actions built from a commit; a rebuild that matches says that
the commit is all that went into it, which anyone can check without trusting the build machine.

## What makes a build the same

The release workflow builds each target with `packaging/release/build.sh` and packs it with
`packaging/release/archive.py`. Between them:

| Input | How it is fixed |
|---|---|
| The compiler | An exact Rust version in `packaging/release/rust-toolchain.toml`, installed with rustup. Dependabot proposes each new Rust release as a pull request, which runs the release dry run and the check below. CI tests with the latest stable and the MSRV; the pin is for release builds |
| Dependencies | `cargo build --locked`: `Cargo.lock` names every crate and its checksum |
| Paths | rustc records source paths in the binary for panic messages: the dependencies' sources under the Cargo home, and the gRPC code that `hidane-proto` generates into the target directory and includes. `build.sh` passes `--remap-path-prefix` so that the checkout reads `/hidane` and the Cargo home `/cargo`, wherever they are; the workspace's own sources are already recorded relative to it. The target directory is always `target/` in the checkout |
| Build metadata | None is compiled in: `hidane --version` prints the crate version only, with no commit, date or host. The third-party notices (`hidane --licenses`) are embedded from a file, which the release generates once with cargo-about and puts in every archive |
| Debug information | The release profile's defaults: no debug information, and `strip = "debuginfo"` removes what the standard library brings. The standard library's own paths are already `/rustc/<commit>/` |
| Windows | `link.exe` writes the link time into the executable and a GUID and path for its PDB unless told otherwise. `build.sh` passes `/Brepro`, which derives them from the contents, and `/PDBALTPATH:%_PDB%`, which keeps only the PDB's file name |
| Archives | `archive.py` sorts the entries, dates them `SOURCE_DATE_EPOCH` (the commit's time in the workflow), owns them by 0:0 without names, gives directories and the binary mode 0755 and everything else 0644, writes ustar, and gzips without a file name and with that same time. The zip for Windows gets the same treatment |

Compiler flags from the environment (`RUSTFLAGS`) are replaced by `build.sh`. A `[profile]`, a
linker or a target directory set in a Cargo configuration of your own (`~/.cargo/config.toml`)
still applies; move it aside to rebuild.

## What is checked

The **Reproducible build** workflow (`.github/workflows/reproducible.yml`) builds each of the five
release targets twice, on two runners, as the release workflow does: one copy checked out where
the release checks out, with the runner's Cargo home; the other in a longer directory, with a
Cargo home of its own. It fails unless the two binaries and the two archives have the same
SHA-256, and writes the digests to its job summary. It runs on pull requests and pushes that
change the build's inputs (`Cargo.lock`, the manifests, build scripts and vendored protos,
`packaging/release/`, the release workflow), weekly for changes on the runners, and by hand.

What has been seen so far (2026-10-13):

| Target | Two builds of one commit with `build.sh`: other directory, other Cargo home, other runner | v0.4.0 rebuilt from its tag on a fresh runner, against the published binary |
|---|---|---|
| `x86_64-unknown-linux-musl` | Same bytes | Same bytes |
| `aarch64-unknown-linux-musl` | Same bytes; also in an `ubuntu:24.04` container on another machine | Same bytes |
| `aarch64-apple-darwin` | Same bytes | Same bytes |
| `x86_64-apple-darwin` | Same bytes | Same bytes |
| `x86_64-pc-windows-msvc` | Same bytes (without `/Brepro` and `/PDBALTPATH`, about 3 million bytes differ) | 24 bytes differ: the link time in the PE headers and the PDB's GUID |

v0.4.0 was built before `build.sh` existed, without remapped paths, so its rebuild had to use the
same paths as the release (the checkout at `/home/runner/work/hidane/hidane` and the Cargo home
of GitHub's runners) and the same Rust (1.99.0). Releases after 0.4.0 are built with `build.sh`
and can be rebuilt anywhere.

## Rebuilding a release

On Linux, for one of the static Linux archives (`<version>` and `<target>` as in the release):

```sh
git clone --branch v<version> https://github.com/hidane-dev/hidane && cd hidane
curl -fsSLO https://github.com/hidane-dev/hidane/releases/download/v<version>/hidane-<version>-<target>.tar.gz
tar -xzf hidane-<version>-<target>.tar.gz
packaging/release/build.sh <target> hidane-<version>-<target>/THIRD-PARTY-LICENSES.txt
sha256sum target/<target>/release/hidane hidane-<version>-<target>/hidane
```

The two digests should be equal. `build.sh` needs rustup, which it uses to install the pinned
Rust and the target, and a C toolchain to link (on Ubuntu, `gcc` and `musl-tools`, as the release
installs). The third-party notices are taken from the release's own archive, so that
`hidane --licenses` prints the same text; the release generates them with cargo-about 0.9.2
(`cargo about generate --locked --fail about.hbs`).

In a container, so that nothing on the machine takes part, the same steps run in `ubuntu:24.04`
(`linux/amd64` for `x86_64`, `linux/arm64` for `aarch64`) after
`apt-get install -y curl ca-certificates gcc libc6-dev musl-tools python3 git` and rustup's
installer with `--profile minimal --default-toolchain none`.

To compare the archive as well, pack the rebuilt binary the way the workflow does, with the
commit's time:

```sh
name=hidane-<version>-<target>
mkdir -p rebuilt/$name
cp target/<target>/release/hidane LICENSE-MIT LICENSE-APACHE README.md $name/THIRD-PARTY-LICENSES.txt rebuilt/$name/
(cd rebuilt && SOURCE_DATE_EPOCH=$(git log -1 --format=%ct) python3 ../packaging/release/archive.py $name $name.tar.gz)
sha256sum rebuilt/$name.tar.gz $name.tar.gz
```

The macOS archives are rebuilt the same way on a Mac, and the Windows archive under Git Bash
(`python` instead of `python3`, `.zip` instead of `.tar.gz`, `hidane.exe`).

## Where it stops

- **The linker and its version.** The Linux binaries are linked by the C toolchain of Ubuntu 24.04
  (the runner images `ubuntu-24.04` and `ubuntu-24.04-arm`), the macOS binaries by the Xcode of
  the `macos-14` image, the Windows binary by the Visual Studio of `windows-2022`. Another
  version of any of them may produce other bytes. The weekly run notices when a runner image
  update does; a rebuild elsewhere is only expected to match with the same toolchain.
- **Compression.** `archive.py` writes the same entries everywhere, but the compressed bytes come
  from the zlib that Python uses, which differs between versions. Compare the binaries when the
  archives differ, and check the archive's contents (`tar -tvzf`) for the rest.
- **macOS signing.** The macOS binaries are not signed with a Developer ID yet. If they are, the
  signature, which holds Apple's timestamp, will make each signed binary unique; a rebuild would
  then be compared with the signature removed (`codesign --remove-signature`).
- **Other channels.** The npm packages, the Homebrew formula and the pub.dev launcher carry or
  fetch the release's binaries, so they are covered through the archives. The container image
  holds the two static Linux binaries, which are, but its layers are built by BuildKit and not
  compared. `cargo install hidane` builds on your machine, with your paths and Rust version, and
  is not expected to match.
