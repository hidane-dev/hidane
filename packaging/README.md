# packaging

What the release workflow (`.github/workflows/release.yml`) turns into hidane's distribution
channels, besides the GitHub Release archives and the crates on crates.io. Each channel installs
the same binaries, built once per release. `docs/releasing.md` has the release steps.

| Directory | Channel | Built how |
|---|---|---|
| `npm/` | npm: `hidane`, with `@hidane/darwin-arm64`, `darwin-x64`, `linux-arm64`, `linux-x64` and `win32-x64` as optional dependencies, so npm installs only the binary for the machine | `npm/assemble.mjs <version> <dist> <out>` builds the six packages from the release archives |
| `pub/` | pub.dev: `hidane`, a Dart launcher. Its first run downloads the release archive for the machine, checks it against `sha256sums.txt` and caches the binary | Published as it is; its version must be the release's |
| `homebrew/` | `brew install hidane-dev/tap/hidane` | `homebrew/formula.mjs <version> <sha256sums.txt>` prints `Formula/hidane.rb` for hidane-dev/homebrew-tap |
| `docker/` | `ghcr.io/hidane-dev/hidane` (linux/amd64, linux/arm64): the static Linux binary on an empty base | `docker/Dockerfile`, with the two musl binaries as the context |

`check-versions.mjs` checks that the workspace `Cargo.toml`, its `=` pins and the pub launcher agree
on the version (CI runs it, and the release workflow checks the tag with it).

Every channel's launcher passes signals sent to it on to hidane and exits with hidane's code, so
supervisors such as firebase-tools can stop it with SIGINT.

The npm and pub.dev names, and the crate `hidane` on crates.io, were first published as 0.0.1
name reservations on 2026-10-09, from the placeholders that lived in `reserve/` until 0.1.0.
