# Listing setup-hidane on GitHub Marketplace

[`hidane-dev/setup-hidane`](https://github.com/hidane-dev/setup-hidane) works with
`uses: hidane-dev/setup-hidane@v1` whether it is listed or not. Listing it on
[GitHub Marketplace](https://github.com/marketplace?type=actions) only makes it findable there,
under the name **Setup hidane** (`https://github.com/marketplace/actions/setup-hidane`).

Publishing to Marketplace has no API: a person does it in the release form on github.com, and
GitHub asks for two-factor authentication when the release is saved. This page is that procedure,
following GitHub's
[Publishing actions in GitHub Marketplace](https://docs.github.com/en/actions/how-tos/create-and-publish-actions/publish-in-github-marketplace).

## Who

An **owner of the `hidane-dev` organization** with two-factor authentication enabled. Only an
organization owner can accept the GitHub Marketplace Developer Agreement for the organization;
after that, anyone who can publish releases in the repository can tick the Marketplace box.

## Requirements

GitHub publishes an action without review when these hold. Checked on 2026-10-13 against v1.0.0:

| Requirement | setup-hidane |
|---|---|
| The repository is public | Yes |
| One action metadata file at the root (`action.yml` or `action.yaml`); others in sub-folders are allowed but not listed | `action.yml` only |
| `name` is unique: not an existing Marketplace action, a GitHub user or organization (unless it is the owner), a Marketplace category, or a reserved GitHub feature name | `Setup hidane`; `marketplace/actions/setup-hidane` and the user `setup-hidane` did not exist |
| `description` is short (Marketplace refuses more than 125 characters) | 119 characters |
| `branding` gives the listing an icon and color | `icon: zap`, `color: orange` |
| The account that owns the repository has accepted the GitHub Marketplace Developer Agreement | Not yet: step 1 below |

If `name` or `description` in `action.yml` changes, check this table again before the next
release.

## Listing the existing v1.0.0 release

1. **Accept the Developer Agreement (once).** Open
   <https://github.com/hidane-dev/setup-hidane/releases/edit/v1.0.0> (Releases, then the pencil
   icon next to *setup-hidane 1.0.0*). Under **Release Action**, the box **Publish this Action to
   the GitHub Marketplace** is disabled until the agreement is accepted; follow its link
   (*accept the GitHub Marketplace Developer Agreement*), read the agreement, accept it for the
   `hidane-dev` organization, and come back to the release page.
2. **Tick the box.** Select **Publish this Action to the GitHub Marketplace**. GitHub checks
   `action.yml`; wait for **Everything looks good!** If it shows an error or a warning instead
   (name taken, description too long, missing branding), stop here: fix `action.yml` in a pull
   request to setup-hidane, release a new patch version (see below) and list that one.
3. **Choose the categories.** **Primary Category:** *Testing*. **Another Category:**
   *Continuous integration*. (setup-hidane installs an emulator used by test suites, mostly in
   CI.)
4. **Leave the rest as it is.** Keep the tag `v1.0.0`, the title *setup-hidane 1.0.0* and the
   release notes. Do not tick *Set as a pre-release*.
5. **Save.** Click **Update release** and complete the two-factor authentication prompt.
6. **Check the listing.**
   - <https://github.com/marketplace/actions/setup-hidane> shows *Setup hidane*, version
     v1.0.0, the zap icon and the README.
   - The repository page shows the Marketplace badge, and the action can be found by searching
     Marketplace for *hidane*.
   - Nothing changes for workflows that already use `hidane-dev/setup-hidane@v1` or a pinned
     commit SHA.

## Later releases

Each release is listed on its own: Marketplace shows the newest release that was published with
the box ticked. `@v1` is a moving tag and is never a release.

1. Merge the change into `main` of setup-hidane, with its `test` workflow green on every runner.
2. Tag the release and move `v1` to it (`v1` must always point at the newest 1.x release):

   ```sh
   git switch main && git pull --ff-only
   git tag -a v1.0.1 -m "v1.0.1"
   git push origin v1.0.1
   git tag -f -a v1 -m "v1 -> v1.0.1" v1.0.1
   git push -f origin v1
   ```

3. Create the release **in the web form**
   (<https://github.com/hidane-dev/setup-hidane/releases/new?tag=v1.0.1>):
   - Tick **Publish this Action to the GitHub Marketplace**.
   - Set the same two categories.
   - Use the title *setup-hidane 1.0.1*, add notes, and click **Publish release**.

   `gh release create` cannot tick the Marketplace box. A release created from the command line
   can be listed afterwards through **Edit**, as in steps 2 to 5 above.
4. A breaking change to inputs or behavior is a new major version: tag `v2.0.0`, start a moving
   `v2` tag, keep `v1` where it is, and say so in setup-hidane's README.

## Removing the listing

Edit each release that was published to Marketplace, untick **Publish this Action to the GitHub
Marketplace** and click **Update release**. The repository, its releases and its tags stay, so
workflows keep working. Deleting the repository would delete the listing and free the name.

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| The Marketplace box is greyed out | The Developer Agreement is not accepted for `hidane-dev`: step 1, by an organization owner |
| No **Release Action** section on the release form | The form was opened in another repository, or `action.yml` is not at the root of the tagged commit |
| "Name is already taken" | Another action or account uses the name; change `name:` in `action.yml`, release a patch version, and update this page |
| "Description is too long" | Shorten `description:` to 125 characters or fewer; the long form belongs in the README |
| Saving asks for a code | Two-factor authentication is required to publish to Marketplace; use the account's 2FA method |
