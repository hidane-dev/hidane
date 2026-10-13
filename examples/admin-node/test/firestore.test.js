// firebase-admin against hidane. `npm test` runs this file under `firebase emulators:exec`, which
// `hidane exec` makes start hidane in place of the official emulator. firebase-tools sets
// FIRESTORE_EMULATOR_HOST, so the Admin SDK talks to hidane, with no credentials.
import assert from 'node:assert/strict';
import { after, beforeEach, test } from 'node:test';
import { initializeApp } from 'firebase-admin/app';
import { FieldValue, getFirestore } from 'firebase-admin/firestore';

// The --project in package.json. A `demo-` project needs no Firebase project or credentials.
const projectId = 'demo-hidane';
const db = getFirestore(initializeApp({ projectId }));

// Each test starts from an empty database: the emulator's own endpoint deletes every document.
beforeEach(async () => {
  const host = process.env.FIRESTORE_EMULATOR_HOST;
  const url = `http://${host}/emulator/v1/projects/${projectId}/databases/(default)/documents`;
  const response = await fetch(url, { method: 'DELETE' });
  assert.equal(response.status, 200);
});

after(() => db.terminate());

test('writes and reads a document', async () => {
  const ref = db.doc('cities/tokyo');
  await ref.set({ name: 'Tokyo', population: 14_000_000, updated: FieldValue.serverTimestamp() });

  const snapshot = await ref.get();
  assert.equal(snapshot.get('name'), 'Tokyo');
  assert.ok(snapshot.get('updated').toMillis() > 0);
});

test('queries, orders and counts', async () => {
  const cities = db.collection('cities');
  await cities.doc('tokyo').set({ name: 'Tokyo', population: 14_000_000 });
  await cities.doc('osaka').set({ name: 'Osaka', population: 2_700_000 });
  await cities.doc('nara').set({ name: 'Nara', population: 350_000 });

  const large = cities.where('population', '>', 1_000_000);
  const snapshot = await large.orderBy('population', 'desc').get();
  assert.deepEqual(snapshot.docs.map((doc) => doc.id), ['tokyo', 'osaka']);
  assert.equal((await large.count().get()).data().count, 2);
});

test('increments a counter in a transaction', async () => {
  const ref = db.doc('counters/visits');
  await ref.set({ n: 41 });

  await db.runTransaction(async (transaction) => {
    const snapshot = await transaction.get(ref);
    transaction.update(ref, { n: snapshot.get('n') + 1 });
  });
  assert.equal((await ref.get()).get('n'), 42);
});

test('a listener receives the documents its query matches', { timeout: 10_000 }, async () => {
  const messages = db.collection('messages');
  const received = new Promise((resolve, reject) => {
    const unsubscribe = messages.where('room', '==', 'general').onSnapshot((snapshot) => {
      if (snapshot.size === 2) {
        unsubscribe();
        resolve(snapshot.docs.map((doc) => doc.get('text')).sort());
      }
    }, reject);
  });

  await messages.add({ room: 'general', text: 'hello' });
  await messages.add({ room: 'random', text: 'elsewhere' });
  await messages.add({ room: 'general', text: 'world' });
  assert.deepEqual(await received, ['hello', 'world']);
});
