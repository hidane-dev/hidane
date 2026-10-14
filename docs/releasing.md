# Releasing

Releases are built by `.github/workflows/release.yml` and published by a maintainer.

## Assets

Each release has one archive per target, with its SBOM, checksum and attestations:

| Target | Archive |
|---|---|
| Linux x86_64, static (musl) | `hidane-<version>-x86_64-unknown-linux-musl.tar.gz` |
| Linux arm64, static (musl) | `hidane-<version>-aarch64-unknown-linux-musl.tar.gz` |
| macOS arm64 | `hidane-<version>-aarch64-apple-darwin.tar.gz` |
| macOS x86_64 | `hidane-<version>-x86_64-apple-darwin.tar.gz` |
| Windows x64 | `hidane-<version>-x86_64-pc-windows-msvc.zip` |

An archive holds one directory, `hidane-<version>-<target>/`, with the `hidane` binary
(`hidane.exe` on Windows), `LICENSE-MIT`, `LICENSE-APACHE`, `THIRD-PARTY-LICENSES.txt` and
`README.md`. Next to each archive, `hidane-<version>-<target>.cdx.json` is its CycloneDX SBOM: the
crates built into the binary (`cargo-cyclonedx`, in the workflow's `sbom` job). `sha256sums.txt`
lists the SHA-256 of every archive and SBOM, as `sha256sum` prints it. Linux binaries are
statically linked, so they run on any distribution, Alpine included.

`THIRD-PARTY-LICENSES.txt` holds the licenses of the crates built into the binary and of the
googleapis protobuf definitions it is generated from; `hidane --licenses` prints the same text.
The release workflow writes it with [cargo-about](https://github.com/EmbarkStudios/cargo-about)
(`about.toml`, `about.hbs`) and embeds it at build time; CI checks on every pull request that
each dependency's license is one `about.toml` accepts.

Each archive carries [GitHub artifact attestations](https://docs.github.com/actions/security-for-github-actions/using-artifact-attestations)
of its build provenance, which ties it to the workflow run and commit that built it, and of its
SBOM; `hidane-<version>.intoto.jsonl` is the provenance's Sigstore bundle. The image on ghcr.io
has the same two attestations. [verifying-releases.md](verifying-releases.md) shows how to check
each of them:

```sh
sha256sum --check --ignore-missing sha256sums.txt
gh attestation verify hidane-<version>-<target>.tar.gz --repo hidane-dev/hidane
```

The binaries and archives are reproducible: `packaging/release/build.sh` builds with the Rust
pinned in `packaging/release/rust-toolchain.toml` and remaps the paths the binary records, and
`packaging/release/archive.py` packs them with fixed dates, owners and modes, so a rebuild of
the tag gives the same bytes. [reproducible-builds.md](reproducible-builds.md) says how to
rebuild a release and compare, what CI checks, and where it stops.

The macOS binaries are not signed or notarized yet. Archives fetched with `curl`, Homebrew or
`gh` run as they are; one downloaded through a browser carries the quarantine attribute, which
`xattr -d com.apple.quarantine hidane` removes ([verifying-releases.md](verifying-releases.md#macos-a-binary-downloaded-with-a-browser)).

## Cutting a release

1. Sort out the release's milestone: close each open issue that is done, with a comment citing
   the evidence (pull requests, tests, files), and move each one that is not to another
   milestone, with a comment saying what remains. Close the milestone once it is empty.
2. Set the version everywhere it is written and merge it: `version` in the workspace
   `Cargo.toml` and the `=` pins of `hidane-core`, `hidane-proto` and `hidane-rules` under
   `[workspace.dependencies]`; `version` in `packaging/pub/pubspec.yaml` and `hidaneVersion` in
   `packaging/pub/lib/hidane.dart`, with a `packaging/pub/CHANGELOG.md` entry.
   `mise run release:check` (`node packaging/check-versions.mjs`) checks that they agree, and CI
   runs it. Run `python3 tools/parity/compatibility.py` too: the compatibility report names the
   workspace version, and CI fails while it is stale.
   The Rust the release builds with is `packaging/release/rust-toolchain.toml`; merge
   Dependabot's pull request for a newer one first if you want it in the release.
3. Push the tag `v<version>` on that commit. The workflow checks the tag against those versions,
   builds every target, writes `sha256sums.txt`, attests the archives and creates a **draft**
   release with all of them.
4. Write the notes on the draft and check the assets. The notes name the official emulator and
   firebase-tools versions the release was verified with, and list any changes to a stable
   surface under **Breaking changes** and **Deprecated** ([versioning.md](versioning.md)).
5. Approve the workflow's `publish the release` job (the `release` environment). It publishes the
   GitHub Release; then, in parallel:

   | Job | Publishes |
   |---|---|
   | `crates.io` | `hidane-proto`, `hidane-core`, `hidane-rules`, then `hidane`; `cargo install` and `cargo binstall` then work |
   | `npm` | **stages** the five `@hidane-dev/<platform>` packages, then `hidane` (`packaging/npm/assemble.mjs`, `stage.sh`), with provenance. Approve them on npmjs.com with 2FA, the platform packages first |
   | `pub.dev` | `packaging/pub` |
   | `ghcr.io/hidane-dev/hidane` | the image for linux/amd64 and linux/arm64, tagged `<version>`, `<major>.<minor>` and `latest`, with provenance and SBOM attestations |
   | `Homebrew tap` | `Formula/hidane.rb` in hidane-dev/homebrew-tap, or the formula in the job summary when `HOMEBREW_TAP_TOKEN` is not set |

   crates.io, npm and pub.dev are reached through trusted publishing: each job gets a
   short-lived token for this workflow, and no registry token is stored in the repository. npm
   is staged rather than published: each version waits on npmjs.com until a maintainer approves
   it with 2FA (from the package's staged versions, or `npm stage list` / `npm stage approve`).
   A failed job can be re-run; the crates.io and npm jobs skip versions that are already published
   (or staged). Re-running a job uses the workflow as it was at the tag; `mise run npm:stage
   v<version>` stages a release's npm packages by hand.
6. Deploy hidane.dev (`mise run site:deploy`, `bun run deploy` in `website/`), whose `install.sh`
   installs the latest release.
7. The Install workflow (`.github/workflows/install.yml`) starts when the Release run ends and
   installs the release through every channel. Its npm, Homebrew and pub.dev jobs wait until
   those channels serve the new version, for up to three hours: approve the staged npm versions
   and, without `HOMEBREW_TAP_TOKEN`, commit the formula within that time.
8. Once every channel serves it, point CI and the docs at the published release, as #282 did:
   `PUBLISHED_VERSION` in `.github/workflows/examples.yml`, the image tag and digest in
   `.github/workflows/testcontainers.yml`, the pinned versions in `docs/release-channels.md` and
   `docs/testcontainers.md`, the placeholder in `.github/ISSUE_TEMPLATE/bug_report.yml` and the
   version in the heading of the README's client table. `git grep` the previous version for the
   rest, and leave what is history (`CHANGELOG.md`, the roadmap, release notes, published results).

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
   GitHub Actions*, repository `hidane-dev/hidane`, workflow `release.yml`, allowed actions
   `npm stage publish` only (direct `npm publish` stays off).
3. **crates.io**: publish `hidane-proto`, `hidane-core` and `hidane-rules` once with an API token,
   at any version before the first release that contains them (a release candidate):
   `mise run crates:bootstrap` (`cargo publish -p hidane-proto`, then `-p hidane-core`, then
   `-p hidane-rules`; versions already on crates.io are skipped). Then, for `hidane`,
   `hidane-core`, `hidane-proto` and `hidane-rules`: *Settings → Trusted Publishing*, repository
   `hidane-dev/hidane`, workflow `release.yml`.
4. **pub.dev**: for `hidane`, *Admin → Automated publishing → Enable publishing from GitHub
   Actions*, repository `hidane-dev/hidane`, tag pattern `v{{version}}`.
5. **ghcr.io**: after the first image is pushed, set the `hidane` package's visibility to public
   (organization *Packages → hidane → Package settings*).

## setup-hidane

The GitHub Action [`hidane-dev/setup-hidane`](https://github.com/hidane-dev/setup-hidane) is
released from its own repository: an annotated tag `v1.x.y` on its `main`, the moving tag `v1`
moved to it, and a GitHub Release created in the web form, which is where a release is listed on
GitHub Marketplace.
Its `migrate` sub-action runs `hidane migrate`, which is in 0.5.0 and newer. Until 0.5.0 was
released, setup-hidane's test workflow built hidane from a commit of `main`; after it, the test
installs the release and setup-hidane v1.1.0 moves `v1`.

[cargo-dist](https://github.com/axodotdev/cargo-dist) was considered (#56) and not adopted: the
workflow above is about one hundred lines that the project controls, with no generator to keep in
step and no installer it does not need yet.
