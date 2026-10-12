# Contributing to hidane

Thanks for your interest in hidane (火種). This document explains where the
project currently stands and how to get involved.

## Project status

v0.2.2 is the current release. hidane serves the Firestore API over gRPC, REST
and WebChannel, enforces Security Rules, and runs under firebase-tools
(`hidane exec -- firebase emulators:start`), with imports and exports. Each
feature is built issue by issue: the official emulator's behaviour is recorded
first (`tools/oracle/`), then implemented and replayed by the test suite. The
Functions triggers (#70) are next.
`packaging/` holds what the release workflow turns into the npm and pub.dev
packages, the Homebrew formula and the container image.

What this means for contributions right now:

- **Open an issue before sending a PR.** Unsolicited pull requests may be
  closed, simply because the code they touch might be about to change shape.
  Issues let us agree on the direction first.
- Findings about the official emulator are very welcome. Use the `research`
  label, or start a thread in GitHub Discussions.
- Observed differences between hidane and the official emulator should be
  filed with the **Parity gap** issue template.

## Development environment

Tooling is managed with [mise](https://mise.jdx.dev/). The pinned versions
live in `mise.toml`.

```sh
mise install   # installs Rust, Node.js and Dart as declared in mise.toml
```

Local-only overrides go in `.env.local` / `.npmrc.local`; both are gitignored
and must never be committed.

## Commit messages

We use [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/):

```
<type>(<optional scope>): <short summary>
```

Common types: `feat`, `fix`, `docs`, `chore`, `refactor`, `test`, `perf`,
`ci`. Scopes usually match the `area:*` labels (for example `grpc`, `rules`,
`webchannel`, `cli`).

```
feat(grpc): implement RunQuery for single-collection filters
fix(rules): evaluate request.time against the server clock
docs: describe import/export directory layout
```

## Pull requests

- Reference the issue the PR resolves.
- Keep PRs focused; one logical change per PR.
- Fill in the PR template, including the **parity impact** section. If the
  change makes hidane behave differently from the official emulator on
  purpose, say so and explain why.
- Leave `packaging/` and the release workflow alone unless the issue is about
  distribution.

## Contributor License Agreement

Contributions are accepted under the hidane Contributor License Agreement
(CLA). **You keep the copyright in your contribution.** The CLA gives the
project a license to use, modify, distribute, sublicense and relicense it,
including under terms other than `MIT OR Apache-2.0` (for example a commercial
edition), and lets the project pass those rights on if it moves to a company
or a new owner. Your contribution as you submitted it stays licensed under
`MIT OR Apache-2.0`, and the patent license is like Apache-2.0's. Why the
project asks for this, what each side keeps and how agreements are recorded:
[docs/cla](docs/cla/README.md).

- **Individual CLA** ([text](docs/cla/individual.md)): for what you contribute
  on your own behalf. On your first pull request the `cla` check fails and a
  comment asks each author of its commits to agree, by posting
  `I have read the hidane Individual CLA 1.0, I have the right to agree to it for my contributions, and I agree to it.`
  on the pull request. Once you have agreed, later pull requests pass without asking again.
- **Corporate CLA** ([text](docs/cla/corporate.md)): for what employees or
  contractors contribute as part of their work for a company. The company
  signs it once and names the GitHub accounts it covers; open an issue titled
  "Corporate CLA" to start. If your employer has rights in what you write,
  check with them before agreeing to the Individual CLA.

If your GitHub profile names a company or you commit with a company email
address, the check labels the pull request `cla: employer check`, and before
merging a maintainer asks whether the contribution is part of your work for
that company; if it is, the company signs the Corporate CLA or confirms in
writing that you may contribute it
([employer check](docs/cla/README.md#employer-check)).

A pull request is merged only when the `cla` check passes. Author your commits
with an email address linked to your GitHub account, so the check can tell who
wrote them. If a commit has human co-authors (`Co-authored-by:`), each of them
must have agreed as well.

## License

hidane is licensed under either of the [Apache License, Version 2.0](./LICENSE-APACHE)
or the [MIT License](./LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions. Separately, its author licenses it to the project under the
[Contributor License Agreement](#contributor-license-agreement).

## Security issues

Please do not open public issues for vulnerabilities. See
[SECURITY.md](./SECURITY.md) for the private reporting process.

## Code of conduct

Participation in this project is governed by our
[Code of Conduct](./CODE_OF_CONDUCT.md).
