// Runs aggregation queries through @google-cloud/firestore (the engine of firebase-admin) and
// prints a transcript, so the transcripts of two emulators can be diffed.
// FIRESTORE_EMULATOR_HOST selects the emulator; start it fresh (or POST /reset) first.
import { AggregateField, Firestore } from '@google-cloud/firestore';

const db = new Firestore({ projectId: 'demo-sdk-aggregations' });
const out = [];
async function step(name, fn) {
  try { out.push({ name, ok: await fn() }); }
  catch (e) { out.push({ name, error: { code: e.code, message: String(e.details ?? e.message).replace(/projects\/[^ "]*/g, '<name>') } }); }
}
const orders = db.collection('orders');
await step('seed', async () => {
  const batch = db.batch();
  const rows = { a: { total: 10, qty: 1, status: 'paid' }, b: { total: 2.5, qty: 3, status: 'paid' }, c: { total: 7, qty: 2, status: 'open' },
    d: { total: 'n/a', qty: 1, status: 'open' }, e: { qty: 5, status: 'void' } };
  for (const [id, row] of Object.entries(rows)) batch.set(orders.doc(id), row);
  batch.set(db.doc('orders/a/lines/1'), { qty: 4 });
  batch.set(db.doc('orders/b/lines/1'), { qty: 6 });
  batch.set(db.doc('big/x'), { n: 9223372036854775807n > 0n ? Number.MAX_SAFE_INTEGER : 0 });
  batch.set(db.doc('big/y'), { n: Number.MAX_SAFE_INTEGER });
  await batch.commit();
  return 'ok';
});
await step('count', async () => (await orders.count().get()).data());
await step('count with a filter', async () => (await orders.where('status', '==', 'open').count().get()).data());
await step('count with a limit', async () => (await orders.limit(2).count().get()).data());
await step('sum and average', async () => (await orders.aggregate({ total: AggregateField.sum('total'), mean: AggregateField.average('total') }).get()).data());
await step('count next to sum', async () => (await orders.aggregate({ n: AggregateField.count(), qty: AggregateField.sum('qty') }).get()).data());
await step('count next to sum of a sparse field', async () => (await orders.aggregate({ n: AggregateField.count(), total: AggregateField.sum('total') }).get()).data());
await step('average of nothing', async () => (await orders.where('status', '==', 'none').aggregate({ mean: AggregateField.average('total'), total: AggregateField.sum('total') }).get()).data());
await step('sum beyond 2^53', async () => (await db.collection('big').aggregate({ n: AggregateField.sum('n') }).get()).data());
await step('collection group count and sum', async () => (await db.collectionGroup('lines').aggregate({ n: AggregateField.count(), qty: AggregateField.sum('qty') }).get()).data());
await step('in a transaction', async () => db.runTransaction(async (tx) => {
  const snap = await tx.get(orders.where('status', '==', 'paid').count());
  tx.set(orders.doc('f'), { total: 1, qty: 1, status: 'paid' });
  return snap.data();
}));
await step('after the transaction', async () => (await orders.where('status', '==', 'paid').count().get()).data());
await step('in a read-only transaction', async () => db.runTransaction(async (tx) => (await tx.get(orders.count())).data(), { readOnly: true }));

console.log(JSON.stringify(out, null, 1));
await db.terminate();
