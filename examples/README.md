# Examples

Small projects whose tests use hidane the way an application's tests would. Each is a starting
point to copy, and an integration test: [`examples.yml`](../.github/workflows/examples.yml) runs
every one of them against hidane built from the pull request and against the published v0.4.0,
for pull requests that change hidane or the examples, and weekly.

| Example | What it shows | How hidane runs |
|---|---|---|
| [`admin-node/`](admin-node/) | firebase-admin with `node:test`: a write and a read, a query with an order and a count, a transaction, a listener | `npm test`: under `firebase emulators:exec`, through `hidane exec` |
| [`admin-python/`](admin-python/) | google-cloud-firestore with pytest: the same four tests | on its own, found through `FIRESTORE_EMULATOR_HOST` |
| [`go/`](go/) | cloud.google.com/go/firestore with `go test`: the same four tests | on its own, found through `FIRESTORE_EMULATOR_HOST` |

Each needs `hidane` on `PATH`, from any channel in the [README](../README.md#install), and uses
the project ID `demo-hidane`: a `demo-` project needs no Firebase project and no credentials. Its
README says how to run it and how CI runs it.

The dependencies are pinned to exact versions, with a lockfile (`package-lock.json`,
`requirements.txt` with hashes, `go.sum`). Dependabot proposes their updates, and this workflow
tests them.
