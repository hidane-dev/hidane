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
(`hidane.exe` on Windows), `LICENSE-MIT`, `LICENSE-APACHE`, `THIRD-PARTY-LICENSES.txt` and
`README.md`. `sha256sums.txt` lists the SHA-256 of every archive, as `sha256sum` prints it. Linux
binaries are statically linked, so they run on any distribution, Alpine included.

`THIRD-PARTY-LICENSES.txt` holds the licenses of the crates built into the binary and of the
googleapis protobuf definitions it is generated from; `hidane --licenses` prints the same text.
The release workflow writes it with [cargo-about](https://github.com/EmbarkStudios/cargo-about)
(`about.toml`, `about.hbs`) and embeds it at build time; CI checks on every pull request that
each dependency's license is one `about.toml` accepts.

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

1. Set the version everywhere it is written and merge it: `version` in the workspace
   `Cargo.toml` and the `=` pins of `hidane-core` and `hidane-proto` under
   `[workspace.dependencies]`; `version` in `packaging/pub/pubspec.yaml` and `hidaneVersion` in
   `packaging/pub/lib/hidane.dart`, with a `packaging/pub/CHANGELOG.md` entry.
   `mise run release:check` (`node packaging/check-versions.mjs`) checks that they agree, and CI
   runs it.
2. Push the tag `v<version>` on that commit. The workflow checks the tag against those versions,
   builds every target, writes `sha256sums.txt`, attests the archives and creates a **draft**
   release with all of them.
3. Write the notes on the draft and check the assets.
4. Approve the workflow's `publish the release` job (the `release` environment). It publishes the
   GitHub Release; then, in parallel:

   | Job | Publishes |
   |---|---|
   | `crates.io` | `hidane-proto`, `hidane-core`, then `hidane`; `cargo install` and `cargo binstall` then work |
   | `npm` | the five `@hidane-dev/<platform>` packages, then `hidane` (`packaging/npm/assemble.mjs`), with provenance |
   | `pub.dev` | `packaging/pub` |
   | `ghcr.io/hidane-dev/hidane` | the image for linux/amd64 and linux/arm64, tagged `<version>`, `<major>.<minor>` and `latest` |
   | `Homebrew tap` | `Formula/hidane.rb` in hidane-dev/homebrew-tap, or the formula in the job summary when `HOMEBREW_TAP_TOKEN` is not set |

   crates.io, npm and pub.dev are reached through trusted publishing: each job gets a
   short-lived token for this workflow, and no registry token is stored in the repository. A
   failed job can be re-run; the crates.io and npm jobs skip versions that are already published.
5. Deploy hidane.dev (`mise run site:deploy`, `bun run deploy` in `website/`), whose `install.sh`
   installs the latest release.

A tag with a pre-release suffix (`v0.1.0-rc.1`) stops at the draft. A pull request that changes the
workflow runs the builds as a dry run; nothing is released.

## One-time setup

Trusted publishing is configured per package on each registry, and a package has to exist before
it can be configured. The registries and the GitHub settings below are set up once, by an owner.

1. **GitHub**: the `release` environment requires a maintainer's approval and accepts tags `v*`
   only. Optionally, a fine-grained token with `contents: write` on hidane-dev/homebrew-tap as the
   repository secret `HOMEBREW_TAP_TOKEN`.
2. **npm**: the organization `hidane-dev` for the `@hidane-dev/` packages. Create the five platform
   packages once, from empty placeholders, while logged in to npm:

   ```sh
   mise run npm:placeholders   # node packaging/npm/assemble.mjs --placeholders 0.0.1 <dir>, then npm publish each
   ```

   Then, for `hidane` and each `@hidane-dev/<platform>` package: *Settings → Trusted publishing →
   GitHub Actions*, repository `hidane-dev/hidane`, workflow `release.yml`.
3. **crates.io**: publish `hidane-proto` and `hidane-core` once with an API token, at any version
   before the first release (a release candidate): `mise run crates:bootstrap` (`cargo publish -p
   hidane-proto`, then `-p hidane-core`). Then, for `hidane`,
   `hidane-core` and `hidane-proto`: *Settings → Trusted Publishing*, repository
   `hidane-dev/hidane`, workflow `release.yml`.
4. **pub.dev**: for `hidane`, *Admin → Automated publishing → Enable publishing from GitHub
   Actions*, repository `hidane-dev/hidane`, tag pattern `v{{version}}`.
5. **ghcr.io**: after the first image is pushed, set the `hidane` package's visibility to public
   (organization *Packages → hidane → Package settings*).

[cargo-dist](https://github.com/axodotdev/cargo-dist) was considered (#56) and not adopted: the
workflow above is about one hundred lines that the project controls, with no generator to keep in
step and no installer it does not need yet.
