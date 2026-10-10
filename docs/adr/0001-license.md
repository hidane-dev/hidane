# ADR 0001: License

- **Status**: Accepted
- **Date**: 2026-10-09 (accepted 2026-10-10)

## Context

The repository and the placeholder packages under `reserve/` use MIT as a placeholder (`LICENSE`,
`reserve/crate/Cargo.toml` `license = "MIT"`, `reserve/npm/package.json` `"license": "MIT"`).
The Rust ecosystem convention is dual `MIT OR Apache-2.0`; the question is whether to add the
Apache-2.0 patent grant. Of the model projects, the two Rust ones (dynoxide, zerobrew) are dual
licensed and the Go one (maestro-runner) is Apache-2.0 only; none documents why.

## Options

### A. Keep MIT

- Pro: simplest; matches the published placeholders
- Con: no explicit patent grant; some corporate users prefer Apache-2.0

### B. MIT OR Apache-2.0 (Rust convention)

- Pro: same as the major Rust crates; users pick; Apache-2.0 carries the patent grant
- Con: two license files; whether npm / pub.dev render the SPDX expression `MIT OR Apache-2.0` correctly is unverified
- Timing: cheapest before any outside contribution (before the first Phase 1 commit)

### C. Apache-2.0 only

- Pro: patent grant
- Con: awkward for MIT-only downstreams; off the Rust convention

## Decision

**B. `MIT OR Apache-2.0`**, before the first release (#76). Every commit so far is the
maintainer's, so the change needs no one else's consent; after outside contributions it would.

- `LICENSE-MIT` and `LICENSE-APACHE` at the root; `license = "MIT OR Apache-2.0"` in the
  workspace `Cargo.toml`, so every crate inherits it.
- The `reserve/` packages changed in the same commit: `license = "MIT OR Apache-2.0"` for the
  crate, `"(MIT OR Apache-2.0)"` for npm (its SPDX expression syntax), and for pub.dev, which
  reads a single `LICENSE` file, both texts in that file. Whether pub.dev's analysis lists both
  is checked when the first real version is published.
- Contributions are dual licensed as above unless stated otherwise (README, `CONTRIBUTING.md`).

## Consequences

- Tracked as a backlog issue (`area:distribution`, `area:docs`)
- Release archives ship `LICENSE-MIT` and `LICENSE-APACHE`
