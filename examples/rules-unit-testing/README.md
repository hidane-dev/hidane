# rules-unit-testing

[`firestore.rules`](firestore.rules) tested with
[`@firebase/rules-unit-testing`](https://www.npmjs.com/package/@firebase/rules-unit-testing) and
the Firebase JS SDK in Node.js, against hidane started by `firebase emulators:exec`.
[`test/rules.test.js`](test/rules.test.js) loads the rules, writes its documents with the rules
disabled, and checks with `assertSucceeds` / `assertFails` what a signed-in user, another user and
a signed-out user may read, list and write; `clearFirestore()` empties the database before each
test.

## Run it

With Node.js 24.12 or newer and `hidane` on `PATH` (any channel in the
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
([ADR 0006](../../docs/adr/0006-launch-under-firebase-tools.md)). rules-unit-testing finds hidane
through the variables firebase-tools sets, and `firebase.json` names the same rules, so that
`firebase emulators:start` enforces them too. `demo-hidane` is a demo project ID, which needs no
Firebase project.

A denial's message can list fewer reasons than the official emulator's; `assertFails` passes the
same ([`docs/parity-exceptions.md`](../../docs/parity-exceptions.md)).

## In CI

[`examples.yml`](../../.github/workflows/examples.yml) runs `npm ci --ignore-scripts` and
`npm test` on Node.js 24, with hidane built from the pull request and with the published v0.4.0.
