# Release channels

Where hidane is released, how to pin a version, and a stable / latest / LTS model hidane could
adopt later. That model is not offered today; this page says what would have to happen first.
What a version number promises is in [versioning.md](versioning.md).

## Today: one line, every channel

There is one release line, the latest release. Every release goes to every channel at once, from
its tag ([releasing.md](releasing.md)). A tag with a pre-release suffix (`v0.5.0-rc.1`) stops at a
draft and reaches no channel. Only the latest release is supported
([versioning.md](versioning.md#before-10)).

### Pinning a version

To upgrade on your own schedule, in CI above all, pin an exact version and raise it deliberately:

| Channel | The latest release | A pinned version |
|---|---|---|
| `install.sh` | `curl -fsSL https://hidane.dev/install.sh \| sh` | `curl -fsSL https://hidane.dev/install.sh \| HIDANE_VERSION=0.5.1 sh` |
| GitHub Releases | [releases/latest](https://github.com/hidane-dev/hidane/releases/latest) | `https://github.com/hidane-dev/hidane/releases/download/v0.5.1/<archive>` |
| GitHub Actions | `uses: hidane-dev/setup-hidane@v1` | `uses: hidane-dev/setup-hidane@v1` with `version: 0.5.1` |
| npm | `npm install --save-dev hidane` | `npm install --save-dev --save-exact hidane@0.5.1` |
| crates.io | `cargo install hidane` | `cargo install hidane --version 0.5.1 --locked` |
| cargo-binstall | `cargo binstall hidane` | `cargo binstall hidane@0.5.1` |
| pub.dev | `dart pub global activate hidane` | `dart pub global activate hidane 0.5.1` |
| Container | `ghcr.io/hidane-dev/hidane:latest` | `ghcr.io/hidane-dev/hidane:0.5.1`, or `:0.5` for the latest patch release of 0.5 |
| mise | `mise use github:hidane-dev/hidane` | `mise use github:hidane-dev/hidane@0.5.1` |
| Homebrew | `brew install hidane-dev/tap/hidane` | Not possible: the tap holds the latest formula only |

## Stable, latest and LTS: a model for later

None of this is offered, scheduled or promised. It is written down so that the project can see
what a longer-lived line would cost, and so that the decision, when it comes, has a starting
point.

| Channel | What it would be |
|---|---|
| `latest` | Every release, as soon as it is published. This is what every channel serves today |
| `stable` | The newest release that has been out for a set time (two weeks, say) without a confirmed regression, for teams that want to wait without tracking version numbers |
| `lts` | One minor line (`1.4.x`, say) that gets patch releases, with security fixes and compatibility fixes that change no stable surface, for a set period (twelve months, say), overlapping the next LTS line |

Each channel would be a name on every channel that has names: an npm dist-tag, an image tag, a
`version:` value of setup-hidane, a value of `install.sh`'s `HIDANE_VERSION`; for an LTS line, a
versioned Homebrew formula (`hidane@1.4`).

### What would start the discussion

Any one of these, and none of them commits the project:

- **hidane 1.0.** Before the stable surfaces in [versioning.md](versioning.md#stable-surfaces)
  stop moving, an LTS line would freeze something still changing.
- **Demand.** Organizations that cannot take every minor release ask for a supported line, in
  [Discussions](https://github.com/hidane-dev/hidane/discussions) or through the contact in the
  README.
- **A split upstream.** A firebase-tools major release, or a new official emulator, that changes
  behaviour many projects cannot follow at once, so that a line verified against the earlier one
  is worth keeping.
- **Capacity.** More than one maintainer, or funding that pays for backports.

### What an LTS line would take

- A release branch per line (`release/1.4`), on which CI, the replay of the oracle recordings and
  the SDK transcripts run as they do on `main`.
- The official emulator and firebase-tools versions pinned on that branch while `main` moves on,
  and each LTS release's notes and documents recording what it was verified against, as for every
  release ([versioning.md](versioning.md#what-each-release-was-verified-against)).
- Backports, and someone accountable for them for the whole period.
- Release automation that can publish an older line's patch release without moving `latest`.
  Today the release workflow marks every release as the latest GitHub Release, tags every image
  `latest`, and stages the npm packages without choosing a dist-tag.

Already in place: every release is versioned and never replaced on any channel, images also
carry a `<major>.<minor>` tag, the release workflow is driven by tags rather than by a branch, and
every archive carries build provenance.
