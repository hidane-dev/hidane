# Contributor License Agreements

hidane accepts contributions under a Contributor License Agreement (CLA). Contributors keep the
copyright in their contributions; the CLA gives the project a license broad enough to use,
distribute, sublicense and relicense them.

| Agreement | For | Agreed by |
|---|---|---|
| [Individual CLA](individual.md) 1.0 | Contributions you make on your own behalf | Commenting on your pull request (below) |
| [Corporate CLA](corporate.md) 1.0 | Contributions your employees or contractors make as part of their work for you | Signature by someone who can bind the company |

## Why

hidane is licensed under `MIT OR Apache-2.0`, and every contribution stays available under those
licenses. The CLA adds what those licenses alone do not give the project: the right to offer the
code under other terms as well (for example a commercial or enterprise edition), to sublicense
it, and to transfer the project, with these rights, to a company or to a new owner. Without it,
each of those steps would need the consent of every past contributor.

| | You (the contributor) | The project |
|---|---|---|
| Copyright | You keep it, and may use and license your contribution in any way | A license, not ownership |
| Use, copy, modify, distribute, sublicense | Yes | Yes: perpetual, worldwide, non-exclusive, royalty-free, irrevocable, transferable, sublicensable |
| License terms | Any | `MIT OR Apache-2.0`, and any other terms, including commercial and proprietary ones |
| What stays open | Your contribution as you submitted it stays licensed under `MIT OR Apache-2.0` | |
| Patents | | A license to the claims your contribution necessarily infringes, as in Apache-2.0, ending for anyone who sues over them |

The agreements are adapted from the [Harmony Agreements](https://www.harmonyagreements.org/)
(CC BY 3.0) and follow the structure of the Apache Software Foundation's CLAs. The counterparty
is the maintainer, Hiroki Nomura, who holds the copyright in the project; the agreements may be
assigned to an entity that takes the project over, which then takes on the maintainer's
obligations under them.

## Agreeing as an individual

When you open your first pull request, the `cla` check fails and a comment lists everyone who
wrote its commits and has not agreed yet: each commit's author, each co-author named in a
`Co-authored-by:` trailer, and the pull request's author. Each of them agrees by posting this
comment on the pull request, from their own GitHub account, exactly as written:

```
I have read the hidane Individual CLA 1.0, I have the right to agree to it for my contributions, and I agree to it.
```

The check then passes, and the agreement covers your later pull requests too. Post `recheck` to
run the check again.

### How authors are identified

The check identifies people only the way GitHub does, and never guesses:

- a commit's author, by the GitHub account its email address is linked to;
- a co-author, by the account GitHub links the trailer's email address to, or by a GitHub private
  address (`<id>+<login>@users.noreply.github.com`) whose account GitHub confirms, ID included.

Someone it cannot identify this way (an address linked to no account, a trailer without an
address) is listed in the comment as needing manual review, and the pull request cannot be
merged until that is fixed: author your commits with an email address linked to your GitHub
account, name co-authors with theirs, and remove a trailer if that person did not write part of
the commit. If someone cannot agree on GitHub, open an issue and we will arrange another way to
sign; once the signed agreement is on file, a maintainer records it with
`/cla signed-offline <email>: <note>`.

### Who needs no agreement

- The maintainers, who hold the copyright in hidane, listed in
  [`.github/cla/exempt`](../../.github/cla/exempt).
- GitHub Apps, such as Dependabot and GitHub Actions, recognised by the account type GitHub
  reports (`Bot`), not by their name.
- Co-authors that are not people, such as an AI coding assistant, listed by email address in
  [`.github/cla/non-human-co-authors`](../../.github/cla/non-human-co-authors). The person who
  commits work written with such a tool contributes it, and their agreement covers it.

These lists are read from the main branch, so a pull request cannot change them for itself.

If your employer has rights in what you write, check with them first: they may need to sign the
Corporate CLA instead (Section 7(d) of the Individual CLA).

## Employer check

Working for a company does not by itself mean it owns what you contribute: what you write on your
own time, on your own equipment and outside your job is usually yours. But a contribution made as
part of your job, or under an employment contract that assigns everything you create to your
employer, is the company's to license, not yours, and the Individual CLA cannot grant it.

A company email address or a company on your profile does not mean the company owns your
contribution: that depends on your employment contract and the law where you work. So the check
does not decide it; it asks, and waits for a maintainer to record the answer.

It looks for signs of an employer: a company on your GitHub profile, commits written with an
email address that is not a personal one (`.github/cla/personal-email-domains` lists the personal
ones), or either sign when you agreed to the CLA (recorded then, so it still counts if your
profile changes). When it finds one, it labels the pull request `cla: employer check`, and the
`cla` check fails with "employer authorization needs review" until one of these is recorded:

| Outcome | When | How it is recorded |
|---|---|---|
| A. Personal contribution | You wrote it outside any work for the company, and the company has no rights in it. You say so in the pull request | A maintainer: `/cla employer @login personal: <note>`, for this pull request |
| B. Employer authorization | The company has rights in it and confirms in writing (an email to the maintainer is enough) that you may contribute it under the Individual CLA | A maintainer: `/cla employer @login authorized: <note>` for this pull request, or `authorized-standing` when the confirmation covers your contributions in general |
| C. Corporate CLA | The company signs the Corporate CLA and names you as a Designated Contributor | Automatic, once you are in its Schedule A |

Only the maintainers' comments are accepted as reviews. The maintainer keeps written
confirmations with the signed agreements; the note says where.

## Signing as a company

Open an issue titled "Corporate CLA" (no confidential details in it), and the maintainer will send
the agreement for signature. Once it is signed by both sides, the GitHub accounts in its
Schedule A pass the `cla` check for the work they do for the company. The company's Point of
Contact changes the list by writing to the maintainer.

## Records

The check records each agreement on the `cla-signatures` branch of this repository, which holds
nothing else:

- `individual.json`: for each contributor, the GitHub account and its numeric ID, the agreement
  and version, the SHA-256 of the agreement's text, the time, the pull request and comment where
  they agreed, and the signs of an employer seen then (the company on their profile, their
  non-personal commit email domains, and whether either was present).
- `corporate.json`: for each company, its name, the agreement and version, the date, and its
  Designated Contributors' GitHub accounts and IDs, with the dates they were added or removed.
  The signed agreements themselves are kept by the maintainer, not in the repository.
- `reviews.json`: each review a maintainer recorded with a `/cla` command: an employer review
  (the account and ID, outcome A or B, whether it covers one pull request or is standing, the
  signs that triggered it) or an agreement made outside GitHub (the email address), each with the
  pull request, the note, who recorded it, when, and the comment. To withdraw a standing record,
  the maintainer adds `"removed": "<date>"` to it.

For a due diligence, these give who agreed to what and when, with the text's SHA-256; the pull
requests that needed the employer check are listed by
`gh pr list --repo hidane-dev/hidane --state all --label "cla: employer check"`, and the outcome of
each is in `reviews.json` or `corporate.json`. The check's own behaviour is tested by
[`.github/cla/test.sh`](../../.github/cla/test.sh), in CI.

## What the CLA does not cover

Code the project takes from others is not a contribution under the CLA, and keeps its own
license: the googleapis `.proto` files vendored in `crates/hidane-proto/proto/` (Apache-2.0), and
the dependencies built into the binary, whose licenses each release lists in
`THIRD-PARTY-LICENSES.txt` (`hidane --licenses`). A
contributor who adds third-party code says so in the pull request, and it is kept with its
license and notices.

## Versions

A new version of an agreement applies only to contributions submitted after agreeing to it; it
gets a new statement to post, and the check asks for it from then on.

## Before the CLA

The CLA was introduced on 2026-10-12, after hidane 0.2.2. Until then every commit and pull request
was the maintainer's own, by the commit authors, the co-author trailers (which name only an AI
assistant), the pull request authors and GitHub's list of contributors: no one else's contribution
had been merged. Contributions made before the CLA are not covered by it unless their author
agrees to it.
