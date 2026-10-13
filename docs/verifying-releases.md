# Verifying a release

What each hidane release publishes to show where its files come from, and how to check them.
From the quickest check to the strongest:

1. **Checksums**: the archive you have is the one the release lists.
2. **Attestations**: the archive, its SBOM and the container image were made by hidane's release
   workflow, from the release's tag, on GitHub-hosted runners. GitHub signs these statements
   with [Sigstore](https://www.sigstore.dev/), and `gh attestation verify` checks them.
3. **A rebuild**: building the tag yourself gives the same bytes
   ([reproducible-builds.md](reproducible-builds.md)).

## What a release publishes

| Asset | What it is |
|---|---|
| `hidane-<version>-<target>.tar.gz` (`.zip` for Windows) | One per target: the binary, the licenses, `THIRD-PARTY-LICENSES.txt`, `README.md` ([releasing.md](releasing.md#assets)) |
| `hidane-<version>-<target>.cdx.json` | The archive's SBOM: every crate built into its binary, with version, license and checksum, in [CycloneDX](https://cyclonedx.org/) 1.5 JSON |
| `sha256sums.txt` | The SHA-256 of every archive and SBOM, as `sha256sum` prints it |
| `hidane-<version>.intoto.jsonl` | The Sigstore bundle of the build provenance attestation that covers every archive and SBOM, for checking without the GitHub API |
| `ghcr.io/hidane-dev/hidane:<version>` | The container image (linux/amd64, linux/arm64), with its own provenance and SBOM attestations |

Every archive since 0.1.0 has a build provenance attestation. The SBOMs, their attestations, the
provenance bundle and the image's attestations start with the first release after 0.4.0.

## Checksums

Download `sha256sums.txt` with the archive, then:

```sh
sha256sum --check --ignore-missing sha256sums.txt               # Linux
shasum -a 256 --check --ignore-missing sha256sums.txt           # macOS
```

On Windows, in PowerShell, compare `(Get-FileHash hidane-<version>-x86_64-pc-windows-msvc.zip).Hash`
with the archive's line in `sha256sums.txt` (the case does not matter).

`install.sh`, Homebrew, setup-hidane and the pub.dev launcher check the archive against
`sha256sums.txt` themselves. A checksum only says the archive is the one listed next to it; the
attestation says who made both.

## Build provenance

With a recent [GitHub CLI](https://cli.github.com/):

```sh
gh attestation verify hidane-<version>-<target>.tar.gz \
  --repo hidane-dev/hidane \
  --signer-workflow hidane-dev/hidane/.github/workflows/release.yml \
  --source-ref refs/tags/v<version> \
  --deny-self-hosted-runners
```

It succeeds only if the archive's digest has an attestation signed for a run of
`.github/workflows/release.yml` in hidane-dev/hidane, started by the tag `v<version>`, on a
GitHub-hosted runner. `--format json` shows the rest of the statement, including the commit it
was built from. setup-hidane runs the same command before it installs a version.

Without `--signer-workflow` and `--source-ref`, `--repo hidane-dev/hidane` alone accepts any
workflow of the repository; the three together pin the release workflow and the version.

To check without the GitHub API (the archive and bundle copied elsewhere, say), use the bundle
attached to the release, and fetch Sigstore's trusted root beforehand for a machine that is
offline:

```sh
gh attestation trusted-root > trusted_root.jsonl         # once, while online
gh attestation verify hidane-<version>-<target>.tar.gz \
  --bundle hidane-<version>.intoto.jsonl \
  --custom-trusted-root trusted_root.jsonl \
  --repo hidane-dev/hidane \
  --signer-workflow hidane-dev/hidane/.github/workflows/release.yml \
  --source-ref refs/tags/v<version>
```

## SBOMs

Each archive's SBOM is on the release next to it, and listed in `sha256sums.txt`. It is also
attested to the archive, so it can be fetched and checked in one step:

```sh
gh attestation verify hidane-<version>-<target>.tar.gz \
  --repo hidane-dev/hidane \
  --signer-workflow hidane-dev/hidane/.github/workflows/release.yml \
  --source-ref refs/tags/v<version> \
  --predicate-type https://cyclonedx.org/bom \
  --format json --jq '.[0].verificationResult.statement.predicate' > hidane.cdx.json
```

The SBOM lists the crates in `Cargo.lock` that the target's binary is built from, build
dependencies and procedural macros included and dev-dependencies left out, as `cargo-cyclonedx`
reads them from `cargo metadata`. Scanners that read CycloneDX, such as OSV-Scanner, Grype or
Trivy, can check it for known vulnerabilities. The binaries do not embed the list themselves, so
a scanner pointed at the binary finds nothing; use the SBOM.

## The container image

The image's attestations are made for its multi-platform index, the digest a tag points to:

```sh
gh attestation verify oci://ghcr.io/hidane-dev/hidane:<version> \
  --repo hidane-dev/hidane \
  --signer-workflow hidane-dev/hidane/.github/workflows/release.yml \
  --source-ref refs/tags/v<version>
```

Add `--predicate-type https://cyclonedx.org/bom` for its SBOM, which is the x86_64 Linux
archive's: the image holds the two static Linux binaries of the release, which are built from the
same crates (the release workflow checks). The attestations are also pushed to ghcr.io next to
the image, where tools that read OCI referrers find them. To be sure of what you run, pin the
digest you verified: `ghcr.io/hidane-dev/hidane@sha256:<digest>`.

## The other channels

| Channel | What ties it to the release |
|---|---|
| npm (`hidane`, `@hidane-dev/<platform>`) | Published by the release workflow through trusted publishing, with npm provenance; `npm audit signatures` checks it for installed packages. The platform packages hold the binaries from the release archives |
| crates.io | Published by the release workflow through trusted publishing. `cargo install` builds from the published sources on your machine, so there is no binary to compare |
| pub.dev | Published by the release workflow (automated publishing from GitHub Actions). The launcher downloads the release archive and checks it against `sha256sums.txt` |
| Homebrew (`hidane-dev/tap/hidane`) | The formula names each archive's SHA-256 from `sha256sums.txt` |
| `install.sh` | Checks the archive against the release's `sha256sums.txt` |
| setup-hidane | Checks `sha256sums.txt` and verifies the build provenance as above |

The macOS binaries are not signed with a Developer ID or notarized; see
[macos-signing.md](macos-signing.md) for what that means for a browser download.

Something does not verify? Do not run it, and report it privately ([SECURITY.md](../SECURITY.md)).
