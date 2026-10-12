// Copied into firebase-js-sdk's packages/firestore/test/ by tools/js-sdk-browser/run.sh, as a
// setup file of the browser tests.
//
// After each test, terminate every Firestore instance it created. Some tests leave theirs
// running (written against production, where it costs nothing), and some lose track of one: a
// test that terminates an instance after starting a new one on the same app unregisters the new
// one, so deleting the app does not reach it. Against an emulator, a running instance keeps its
// Listen and Write streams open, each holding an HTTP/1.1 connection, and Chromium opens at most
// six connections to one host and port: after three such instances, the tests that follow wait
// for a connection until they time out.
import { afterEach } from 'vitest';

import { Firestore } from '../src/api/database';

const instances = new Set<Firestore>();

// Every instance gets its settings once, when it is created.
// eslint-disable-next-line @typescript-eslint/unbound-method
const setSettings = Firestore.prototype._setSettings;
Firestore.prototype._setSettings = function (this: Firestore, settings) {
  instances.add(this);
  return setSettings.call(this, settings);
};

afterEach(async () => {
  const left = [...instances];
  instances.clear();
  // `_delete()` is what `terminate()` does after unregistering the instance from its app, which
  // may be deleted by now; for an instance terminated already, it returns the same promise.
  await Promise.all(left.map(db => db._delete().catch(() => {})));
});
