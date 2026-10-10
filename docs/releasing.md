# Releasing

Releases are built by `.github/workflows/release.yml` and published by a maintainer.

## Assets

Each release has one archive per target, its checksum and its build provenance:

| Target | Archive |
|---|---|
| Linux x86_64, static (musl) | `hidane-<version>-x86_64-unknown-linux-musl.tar.gz` |
| Linux arm64, static (musl) | `hidane-<version>-aarch64-unknown-linux-musl.tar.gz` |
| macOS arm64 | `hidane-<version>-aarch64-apple-darwin.tar.gz` |
| macOS x86_64 | `hidane-<version>-x86_64-apple-darwin.tar.gz` |
| Windows x64 | `hidane-<version>-x86_64-pc-windows-msvc.zip` |

An archive holds one directory, `hidane-<version>-<target>/`, with the `hidane` binary
(`hidane.exe` on Windows), `LICENSE-MIT`, `LICENSE-APACHE` and `README.md`. `sha256sums.txt` lists the SHA-256 of every
archive, as `sha256sum` prints it. Linux binaries are statically linked, so they run on any
distribution, Alpine included.

Each archive carries a [GitHub artifact attestation](https://docs.github.com/actions/security-for-github-actions/using-artifact-attestations)
of its build provenance, which ties it to the workflow run and commit that built it:

```sh
sha256sum --check --ignore-missing sha256sums.txt
gh attestation verify hidane-<version>-<target>.tar.gz --repo hidane-dev/hidane
```

The macOS binaries are not signed or notarized yet. Archives fetched with `curl`, Homebrew or
`gh` run as they are; one downloaded through a browser carries the quarantine attribute, which
`xattr -d com.apple.quarantine hidane` removes.

## Cutting a release

1. Set `version` in the workspace `Cargo.toml` and merge it.
2. Push the tag `v<version>` on that commit. The workflow checks the tag against the crate version,
   builds every target, writes `sha256sums.txt`, attests the archives and creates a **draft**
   release with all of them.
3. Write the notes on the draft, check the assets, and publish it.

A pull request that changes the workflow runs the builds as a dry run; nothing is released.

## Not decided here

- The license before the first release ([ADR 0001](adr/0001-license.md), #76).
- Further channels (Homebrew tap, `cargo binstall`, npm, pub.dev, a container image, `curl | sh`)
  build on these assets: #77, #78, #79. `cargo binstall` also needs the crate published, which
  `publish = false` prevents today.

[cargo-dist](https://github.com/axodotdev/cargo-dist) was considered (#56) and not adopted: the
workflow above is about one hundred lines that the project controls, with no generator to keep in
step and no installer it does not need yet.
