# ADR 0007: Contributor License Agreement

- **Status**: Accepted
- **Date**: 2026-10-12

## Context

hidane is licensed under `MIT OR Apache-2.0` (ADR 0001), and until now a contribution was accepted
on those terms alone (Apache-2.0, Section 5: inbound = outbound). That lets anyone, the project
included, use a contribution under those two licenses, but nothing more: offering the code under
other terms (a commercial or enterprise edition, an open-core split, a different license), or
moving the project with such rights to a company or a buyer, would need the consent of every
contributor. hidane may do any of these, and an acquirer's IP due diligence asks who wrote the
code and under which grant.

Until this ADR, every commit and pull request was the maintainer's own (commit authors and
committers, `Co-authored-by` trailers, which name only an AI assistant, pull request authors and
GitHub's contributor list), so a CLA introduced now covers every outside contribution from the
first one, and nothing has to be applied retroactively.

## Options

### Agreement

- **A. Copyright assignment.** The project owns everything. Unpopular with contributors and with
  their employers; more than the project needs.
- **B. DCO only.** Certifies the contributor's right to submit under the project license; grants
  no right to relicense.
- **C. License CLA** (Apache ICLA / CCLA, Google, Harmony). The contributor keeps the copyright
  and grants a broad, perpetual, irrevocable, sublicensable license with an Apache-2.0-style
  patent license. With Harmony's "any license" outbound option, the project may relicense,
  and the contributor is promised that the contribution stays available under the project
  license.

### Collecting agreement

- **cla-assistant.io** (SAP, free GitHub App): no code in the repository; records live in SAP's
  database (exportable as CSV); the upstream project's last commit is from 2023.
- **CLA Assistant Lite** (GitHub Action): records in the repository; archived on 2026-03-23.
- **A workflow of the project's own**: about 150 lines of shell (`gh`, `jq`) with records in this
  repository's git history.

## Decision

**C, with a workflow of the project's own.**

- [`docs/cla/individual.md`](../cla/individual.md) and [`docs/cla/corporate.md`](../cla/corporate.md),
  version 1.0, adapted from the Harmony Agreements (HA-CLA-I / HA-CLA-E 1.0, CC BY 3.0, with
  attribution) and following the Apache ICLA / CCLA structure: copyright license, any outbound
  license with the contribution kept under the project licenses, patent license limited to
  necessarily infringed claims with termination on litigation, moral rights not asserted,
  representations to the best of the contributor's knowledge, assignable to a successor that
  takes on the obligations, governed by Japanese law.
- The counterparty is the maintainer, Hiroki Nomura, who holds the copyright (`LICENSE-MIT`):
  there is no legal entity behind hidane yet, and the agreements may be assigned to one.
- `.github/workflows/cla.yml` runs `.github/cla/check.sh` on `pull_request_target` and on
  comments: it sets the `cla` commit status, comments once, and records each agreement (account,
  numeric ID, version, SHA-256 of the text, pull request and comment) on the `cla-signatures`
  branch; corporate agreements are added there by the maintainer. It never checks out or runs a
  pull request's code. The main branch requires the `cla` status.
- `MIT OR Apache-2.0` is unchanged, and so is the inbound license to everyone.

## Consequences

- CONTRIBUTING.md, the README and the pull request template describe the CLA; `docs/cla/README.md`
  explains it to contributors.
- An employee may agree to the Individual CLA for work their employer owns. The statement
  confirms the right to agree; a company on the author's profile, or a non-personal commit email
  domain, labels the pull request `cla: employer check`, and a maintainer confirms with the author,
  and if needed the company, before merging. The signs are recorded with each agreement. Since
  2026-10-13 the check enforces this (below).
- The repository settings must require the `cla` status on `main` and protect the
  `cla-signatures` branch from deletion and force pushes.
- When the project moves to a legal entity, the maintainer assigns the agreements to it, and a
  new version names the entity for later contributions.
- Before relying on the agreements commercially, a lawyer should review them, in particular the
  governing law, the outbound license clause and the patent clause.

## Update (2026-10-13): co-authors and the employer review are enforced

A review of the check found two gaps between what the documents promised and what the check
did:

- It identified a pull request's authors by the GitHub account of each commit's author only, so a
  person named in a `Co-authored-by:` trailer was never asked to agree, although CONTRIBUTING.md
  required it. The check now reads the co-authors GitHub lists and the trailers themselves, and
  asks each of them. It identifies someone only by an email address GitHub links to their
  account, or by a GitHub private address whose account GitHub confirms; it does not guess, and
  anyone it cannot identify blocks the pull request until fixed or recorded by a maintainer.
- A sign of an employer only labelled the pull request: the `cla` status passed once the author
  agreed, and the maintainer's confirmation was a manual step with no record. The status now
  fails until the outcome is recorded: the contribution is the author's own, the employer
  authorized it, or the author is a Designated Contributor under a Corporate CLA. A sign does not
  decide ownership; it asks for a review. Reviews are recorded on the `cla-signatures` branch
  (`reviews.json`), by the maintainers only.

GitHub Apps are now told apart by the account type GitHub reports rather than by a name ending in
`[bot]`, and co-authors that are not people (an AI coding assistant) are listed by email on the
main branch. `.github/cla/test.sh` tests the check against a fake `gh` in CI. Nothing changed for
agreements already recorded, and the agreements themselves are unchanged.
