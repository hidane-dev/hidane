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

When you open your first pull request, the `cla` check fails and a comment lists every author of
its commits who has not agreed yet. Each of them agrees by posting this comment on the pull
request, from their own GitHub account, exactly as written:

```
I have read the hidane Individual CLA 1.0, I have the right to agree to it for my contributions, and I agree to it.
```

The check then passes, and the agreement covers your later pull requests too. Commits must be
authored with an email address linked to your GitHub account, so that the check can tell who
wrote them. Post `recheck` to run the check again. If you cannot agree on GitHub, open an issue
and we will arrange another way to sign.

If your employer has rights in what you write, check with them first: they may need to sign the
Corporate CLA instead (Section 7(d) of the Individual CLA).

## Employer check

Working for a company does not by itself mean it owns what you contribute: what you write on your
own time, on your own equipment and outside your job is usually yours. But a contribution made as
part of your job, or under an employment contract that assigns everything you create to your
employer, is the company's to license, not yours, and the Individual CLA cannot grant it.

So the check looks for signs of an employer: a company on your GitHub profile, or commits authored
with an email address that is not a personal one (`.github/cla/personal-email-domains` lists the
personal ones). When it finds one, it does not fail; it labels the pull request
`cla: employer check` and adds a note to its comment. Before merging such a pull request, unless
it is trivial (a typo, a one-line fix), a maintainer asks whether it is part of your work for that
company, and records the answer in the pull request:

- it is your own work, outside any work for the company: your statement in the pull request is
  enough;
- it is work for the company, or the company has rights in it: the company signs the Corporate
  CLA, or confirms in writing (an email to the maintainer is enough) that you may contribute it
  under the Individual CLA. The maintainer keeps that confirmation with the signed agreements.

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

The maintainer and bots (accounts ending in `[bot]`) are exempt; the maintainer's accounts are
listed in [`.github/cla/exempt`](../../.github/cla/exempt).

For a due diligence, these give who agreed to what and when; the pull requests that needed the
employer check, with the maintainer's question and the answer in each, are listed by
`gh pr list --repo hidane-dev/hidane --state all --label "cla: employer check"`.

## Versions

A new version of an agreement applies only to contributions submitted after agreeing to it; it
gets a new statement to post, and the check asks for it from then on.

## Before the CLA

The CLA was introduced on 2026-10-12, after hidane 0.2.2. Until then every commit and pull request
was the maintainer's own, by the commit authors, the co-author trailers (which name only an AI
assistant), the pull request authors and GitHub's list of contributors: no one else's contribution
had been merged. Contributions made before the CLA are not covered by it unless their author
agrees to it.
