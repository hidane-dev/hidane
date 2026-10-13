// The function in functions/index.js, run by the Functions emulator on a write to hidane. `npm test`
// runs this file under `firebase emulators:exec --only functions,firestore`, which `hidane exec`
// makes start hidane in place of the official Firestore emulator.
import assert from 'node:assert/strict';
import { after, test } from 'node:test';
import { initializeApp } from 'firebase-admin/app';
import { getFirestore } from 'firebase-admin/firestore';

// The --project in package.json. A `demo-` project needs no Firebase project or credentials.
const db = getFirestore(initializeApp({ projectId: 'demo-hidane' }));

after(() => db.terminate());

test('a new message gets its text in upper case', { timeout: 30_000 }, async () => {
  const ref = await db.collection('messages').add({ text: 'hello, hidane' });

  // The first call loads the function, which can take a few seconds: wait for its write.
  const uppercase = await new Promise((resolve, reject) => {
    const unsubscribe = ref.onSnapshot((snapshot) => {
      if (snapshot.get('uppercase') !== undefined) {
        unsubscribe();
        resolve(snapshot.get('uppercase'));
      }
    }, reject);
  });
  assert.equal(uppercase, 'HELLO, HIDANE');
});
