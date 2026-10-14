# Examples

Small projects whose tests use hidane the way an application's tests would. Each is a starting
point to copy, and an integration test: [`examples.yml`](../.github/workflows/examples.yml) runs
every one of them against hidane built from the pull request and against the published release
that it pins (`PUBLISHED_VERSION`; the GitHub Actions workflow, against the latest release, as it
installs), for pull requests that
change hidane or the examples, and weekly.

| Example | What it shows | How hidane runs |
|---|---|---|
| [`admin-node/`](admin-node/) | firebase-admin with `node:test`: a write and a read, a query with an order and a count, a transaction, a listener | `npm test`: under `firebase emulators:exec`, through `hidane exec` |
| [`functions/`](functions/) | a Firestore-triggered Cloud Function (2nd gen) under the Functions emulator | `npm test`: under `firebase emulators:exec --only functions,firestore`, through `hidane exec` |
| [`rules-unit-testing/`](rules-unit-testing/) | `firestore.rules` tested with `@firebase/rules-unit-testing` and the Firebase JS SDK | `npm test`: under `firebase emulators:exec`, through `hidane exec` |
| [`admin-python/`](admin-python/) | google-cloud-firestore with pytest: the same four tests as admin-node | on its own, found through `FIRESTORE_EMULATOR_HOST` |
| [`go/`](go/) | cloud.google.com/go/firestore with `go test`: the same four tests as admin-node | on its own, found through `FIRESTORE_EMULATOR_HOST` |
| [`github-actions/`](github-actions/) | a workflow to copy: hidane installed by `hidane-dev/setup-hidane`, then `npm test` | as the project's tests start it |

Each needs `hidane` on `PATH`, from any channel in the [README](../README.md#install), and uses
the project ID `demo-hidane`: a `demo-` project needs no Firebase project and no credentials. Its
README says how to run it and how CI runs it.

The dependencies are pinned to exact versions, with a lockfile (`package-lock.json`,
`requirements.txt` with hashes, `go.sum`). Dependabot proposes their updates, and this workflow
tests them.

Where a pinned package holds a vulnerable transitive dependency that Dependabot cannot move, the
Node examples override it, scoped to that package, at the first fixed version: `uuid` 11.1.1 under
firebase-tools' `gaxios`, and `@grpc/grpc-js` 1.14.6 under `@firebase/firestore` (firebase 13 pins
1.9). Remove an override once the package that pins the old version moves past it.

Not here: a Next.js app would reach Firestore through the same SDKs as `admin-node/` (on the
server) and `rules-unit-testing/` (the Firebase JS SDK), so it would add a framework and a build
without testing anything more. Flutter Web needs the Flutter SDK and a browser; it is verified in
[`tools/oracle/flutter_web/`](../tools/oracle/flutter_web/), and Flutter's other platforms are not
verified yet.
