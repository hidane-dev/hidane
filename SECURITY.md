# Security Policy

## Supported versions

hidane is at 0.x. Only the latest release is supported: a security fix goes into the next
release, and nothing is backported to an older minor line. Upgrade to the latest release to get
the fix.

| Version | Security fixes |
|---|---|
| The latest release ([releases/latest](https://github.com/hidane-dev/hidane/releases/latest)) | Yes |
| Any earlier release | No; upgrade |
| `main` between releases | Fixed there first, released with the next version |

The same holds on every channel (GitHub Releases, crates.io, npm, pub.dev, Homebrew,
`ghcr.io/hidane-dev/hidane`), since each one carries the same release.

## Reporting a vulnerability

Please do **not** open a public issue, pull request or discussion for a security problem.

Report it privately through GitHub's private vulnerability reporting:
<https://github.com/hidane-dev/hidane/security/advisories/new>. Only the maintainers see the
report, and the conversation, the fix and the advisory are worked on in the same place.

A useful report says:

- the hidane version (`hidane --version`) and how it was installed;
- what an attacker can do, and from where (a local process, a page open in the browser, the
  network);
- the steps or a request that shows it, and what you expected instead.

## What to expect

hidane is maintained by a small team, so there is no guaranteed response time. What we aim for:

- an acknowledgement within a week, and a first assessment (accepted, needs more information, or
  out of scope with the reason) soon after;
- a fix in a release as soon as one is ready, with a GitHub Security Advisory that describes the
  problem, the affected and fixed versions, and what to do. When it warrants one, we request a
  CVE through the advisory;
- credit in the advisory, unless you ask us not to name you.

Please keep the details private until the advisory is published, or for 90 days after your
report if that comes first. If you have not heard from us within two weeks, a reminder on the
same report is welcome.

## Scope

hidane is a local development emulator. It is not designed to be exposed to untrusted networks,
and reports about running it on a public interface without authentication are out of scope.

In scope:

- the gRPC, REST and WebChannel surface, including what a web page open in the developer's
  browser can make a local hidane do;
- the Security Rules evaluator, where it allows something the rules deny;
- data import and export, and the files hidane reads or writes;
- `hidane exec`, `hidane doctor` and `hidane migrate`, and the files they read or change;
- the installer (`https://hidane.dev/install.sh`), the release archives, the packages on each
  channel and the container image, and the workflows that build and publish them.

Out of scope: the official Firestore emulator, firebase-tools and the Firebase SDKs (please
report those to Google), and what hidane does like the official emulator by design: it accepts
unsigned ID tokens, and a caller sending `Authorization: Bearer owner` is not subject to Security
Rules.

## Verifying what you install

Each release lists the SHA-256 of its archives and carries GitHub artifact attestations of their
build provenance and SBOMs; the container image is attested too, and the binaries can be rebuilt
byte for byte from the tag. [docs/verifying-releases.md](docs/verifying-releases.md) shows how to
check each of them. If something does not verify, do not run it, and report it as above.
