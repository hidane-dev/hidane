// Firestore-triggered functions under firebase-tools' Functions emulator (#70): writes documents
// that `functions/index.js` watches (creates, updates, a write that changes nothing, a merge,
// deletes, the delete of a missing document, a batch, a transaction, a subcollection, a named
// database, a collection nothing watches), waits until the functions have run, then prints what
// each of them received, sorted. Run it from a copy of this directory outside the repository,
// once per emulator, and diff the transcripts:
//
//   npm i firebase-admin && (cd functions && npm i)
//   firebase emulators:exec --only functions,firestore --project demo-functions 'node drive.mjs' > official.out
//   hidane exec -- firebase emulators:exec --only functions,firestore --project demo-functions 'node drive.mjs' > hidane.out
//
// firebase-tools prints its own lines around the transcript; the transcript is the JSON between
// the lines `--- transcript ---` and `--- end ---`.
import { initializeApp } from 'firebase-admin/app';
import { FieldValue, getFirestore } from 'firebase-admin/firestore';

const app = initializeApp({ projectId: process.env.GCLOUD_PROJECT || 'demo-functions' });
const db = getFirestore(app);
const named = getFirestore(app, 'named-db');

await db.doc('items/a').set({ n: 1, s: 'x' });
await db.doc('items/a').update({ n: 2 });
await db.doc('items/a').set({ n: 2, s: 'x' }); // changes nothing
await db.doc('items/a').set({ m: 3 }, { merge: true });
await db.doc('items/a').delete();
await db.doc('items/missing').delete(); // nothing to delete
const batch = db.batch();
batch.set(db.doc('items/b'), { n: 1 });
batch.set(db.doc('items/c'), { n: FieldValue.increment(5) });
await batch.commit();
await db.runTransaction(async (t) => {
  const b = await t.get(db.doc('items/b'));
  t.update(db.doc('items/b'), { n: b.get('n') + 1 });
});
await db.doc('items/b/sub/x').set({ n: 1 });
await named.doc('items/a').set({ n: 1 });
await db.doc('other/a').set({ n: 1 });

// Until no function has written anything for five seconds (the first call loads the functions).
let count = -1;
let quiet = 0;
for (let waited = 0; waited < 120 && quiet < 10; waited++) {
  await new Promise((resolve) => setTimeout(resolve, 500));
  const now = (await db.collection('received').count().get()).data().count;
  quiet = now === count ? quiet + 1 : 0;
  count = now;
}

const received = (await db.collection('received').get()).docs
  .map((doc) => ({ trigger: doc.get('trigger'), ...JSON.parse(doc.get('info')) }))
  .map((r) => JSON.stringify(r))
  .sort()
  .map((r) => JSON.parse(r));
console.log('--- transcript ---');
console.log(JSON.stringify(received, null, 1));
console.log('--- end ---');
await db.terminate();
await named.terminate();
