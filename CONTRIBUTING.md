# Contributing to hidane

Thanks for your interest in hidane (火種). This document explains where the
project currently stands and how to get involved.

## Project status: pre-release

The emulator runs from source (`cargo run --release -p hidane`) and serves the
Firestore API over gRPC, REST and WebChannel. Each feature is built issue by
issue: the official emulator's behaviour is recorded first (`tools/oracle/`),
then implemented and replayed by the test suite; `hidane exec -- firebase
emulators:start` runs it under firebase-tools, with imports and exports.
Security Rules are still ahead. `packaging/` holds what the release workflow turns
into the npm and pub.dev packages, the Homebrew formula and the container image.

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

## DCO / CLA

No Developer Certificate of Origin sign-off and no Contributor License
Agreement are required. By contributing you agree that your contribution is
licensed under the project license below.

## License

hidane is licensed under either of the [Apache License, Version 2.0](./LICENSE-APACHE)
or the [MIT License](./LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.

## Security issues

Please do not open public issues for vulnerabilities. See
[SECURITY.md](./SECURITY.md) for the private reporting process.

## Code of conduct

Participation in this project is governed by our
[Code of Conduct](./CODE_OF_CONDUCT.md).
