# github-actions

[`firestore-tests.yml`](firestore-tests.yml), a GitHub Actions workflow to copy into a
repository's `.github/workflows/`. It installs hidane with
[`hidane-dev/setup-hidane`](https://github.com/hidane-dev/setup-hidane), which checks the release
archive's SHA-256 and build provenance, then runs `npm ci` and `npm test` on Node.js 24. No Java is
set up: `npm test` starts hidane through `hidane exec`, as in [`admin-node/`](../admin-node/) and
the other Node.js examples.

The actions are pinned by commit SHA, with the version in a comment, which Dependabot keeps
current; `hidane-dev/setup-hidane@v1` would follow the latest v1 release instead. setup-hidane
installs the latest hidane release unless `version` pins one.

For a project that starts hidane on its own, as [`admin-python/`](../admin-python/) and
[`go/`](../go/) do, replace `npm test` with a step that starts it, and set
`FIRESTORE_EMULATOR_HOST` for the tests:

```yaml
- name: Start hidane
  run: |
    hidane --host 127.0.0.1 --port 8080 > hidane.log 2>&1 &
    curl -fsS --retry 30 --retry-connrefused --retry-delay 1 http://127.0.0.1:8080/
- run: pytest
  env:
    FIRESTORE_EMULATOR_HOST: 127.0.0.1:8080
```

## In CI

The `github-actions` job of [`examples.yml`](../../.github/workflows/examples.yml) runs the same
steps, with the same actions, in [`admin-node/`](../admin-node/), and fails if this file's actions
or commands differ from its own. Like the workflow, it installs the latest release.
