# functions

A Firestore-triggered Cloud Function (2nd gen) under the Functions emulator, with hidane as the
Firestore emulator. [`functions/index.js`](functions/index.js) writes a new message's text in
upper case into the same document; [`test/uppercase.test.js`](test/uppercase.test.js) adds a
message through firebase-admin and waits, with a listener, until the function has written.

## Run it

With Node.js 24 and `hidane` on `PATH` (any channel in the [README](../../README.md#install)):

```sh
npm ci
npm ci --prefix functions
npm test
```

`npm test` runs

```sh
hidane exec -- firebase emulators:exec --only functions,firestore --project demo-hidane "node --test"
```

`hidane exec` makes firebase-tools, a devDependency here, start hidane where it would start the
official Firestore emulator's jar: no Java is needed and no jar is downloaded
([ADR 0006](../../docs/adr/0006-launch-under-firebase-tools.md)). The Functions emulator loads
`functions/` (firebase-functions 7.4.0) and registers its trigger with hidane, which sends it each
matching write. `demo-hidane` is a demo project ID, which needs no Firebase project.

## In CI

[`examples.yml`](../../.github/workflows/examples.yml) runs `npm ci --ignore-scripts` in both
directories and `npm test` on Node.js 24, with hidane built from the pull request and with the
published release that it pins (`PUBLISHED_VERSION`).
