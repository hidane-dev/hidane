// The README's write demo: 100,000 documents written to the emulator at FIRESTORE_EMULATOR_HOST in
// commits of 500, one commit at a time, with the count so far, then the time from the first commit
// sent to the last one answered. The documents are the canonical benchmark's (`seed_100000`); the
// client is firebase-admin rather than the benchmark's own.
import { initializeApp } from "firebase-admin/app";
import { getFirestore } from "firebase-admin/firestore";

const total = 100_000;
const batchSize = 500;

initializeApp({ projectId: "demo-hidane" });
const db = getFirestore();
// The connection is open before the clock starts.
await db.doc("items/none").get();

const start = performance.now();
for (let n = 0; n < total; n += batchSize) {
  const batch = db.batch();
  for (let i = n; i < n + batchSize; i++) {
    batch.set(db.doc(`items/doc-${i}`), { mykey: `key${i}`, myid: i, bucket: i % 100 });
  }
  await batch.commit();
  process.stdout.write(`\r${(n + batchSize).toLocaleString("en-US")} documents written`);
}
const seconds = (performance.now() - start) / 1000;
process.stdout.write(`\r${total.toLocaleString("en-US")} documents written in ${seconds.toFixed(1)} s\n`);
await db.terminate();
