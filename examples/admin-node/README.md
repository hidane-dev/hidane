# admin-node

[firebase-admin](https://www.npmjs.com/package/firebase-admin) with Node.js's built-in test
runner, against hidane started by `firebase emulators:exec`.
[`test/firestore.test.js`](test/firestore.test.js) writes and reads a document, queries, orders
and counts, runs a transaction and listens to a query, each test on an empty database.

## Run it

With Node.js 22 or newer and `hidane` on `PATH` (any channel in the
[README](../../README.md#install)):

```sh
npm ci
npm test
```

`npm test` runs

```sh
hidane exec -- firebase emulators:exec --only firestore --project demo-hidane "node --test"
```

`hidane exec` makes firebase-tools, a devDependency here, start hidane where it would start the
official emulator's jar: no Java is needed and no jar is downloaded
([ADR 0006](../../docs/adr/0006-launch-under-firebase-tools.md)). firebase-tools sets
`FIRESTORE_EMULATOR_HOST` for the tests, and the Admin SDK connects to that address without
credentials. `demo-hidane` is a demo project ID, which needs no Firebase project.

To pin hidane with the other dependencies, `npm install --save-dev hidane`: npm scripts run the
binary in `node_modules/.bin` before the one on `PATH`.

## In CI

[`examples.yml`](../../.github/workflows/examples.yml) runs `npm ci --ignore-scripts` and
`npm test` on Node.js 24, with hidane built from the pull request and with the published release
that it pins (`PUBLISHED_VERSION`).
Its `github-actions` job runs this project with the workflow in
[`github-actions/`](../github-actions/), which you can copy.
