# ADR 0001: License

- **Status**: Draft (undecided)
- **Date**: 2026-10-09

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

Undecided. Placeholder: MIT (current). Revisit B before Phase 1 starts. If changed, update
`LICENSE` and the three `reserve/` package manifests in the same commit.

## Consequences

- Tracked as a backlog issue (`area:distribution`, `area:docs`)
- `CONTRIBUTING.md` already states that the license may change
