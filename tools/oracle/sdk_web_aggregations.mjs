// Runs aggregation queries through the firebase-js-sdk (`firebase/firestore`, the web SDK, here in
// Node, over gRPC: RunAggregationQuery) and prints a transcript, so the transcripts of two
// emulators can be diffed. The first entry records the SDK's version.
//
// Run it from a directory outside the repository where `npm i firebase@13.0.0` was run (the
// version of the other web SDK transcripts in Node), against a freshly started emulator:
//   FIRESTORE_EMULATOR_HOST=127.0.0.1:8089 node sdk_web_aggregations.mjs > official.json
//
// The web SDK has no aggregations in transactions (`Transaction.get` takes a document reference
// only) and no `count({ upTo })`, so neither is here. Aggregate fields are printed with their
// keys sorted, non-finite numbers as strings (JSON has none).
import { initializeApp, SDK_VERSION } from 'firebase/app';
import {
  and, average, collection, collectionGroup, connectFirestoreEmulator, count, deleteDoc, doc,
  documentId, getAggregateFromServer, getCountFromServer, getFirestore, limit, limitToLast, or,
  orderBy, query, setDoc, startAfter, sum, terminate, where, writeBatch,
} from 'firebase/firestore';

const [host, port] = process.env.FIRESTORE_EMULATOR_HOST.split(':');
const app = initializeApp({ projectId: 'demo-web-aggregations', apiKey: 'demo' });
const db = getFirestore(app);
connectFirestoreEmulator(db, host, Number(port));

const out = [{ versions: { firebase: SDK_VERSION } }];
async function step(name, fn) {
  try { out.push({ name, ok: await fn() }); }
  catch (e) { out.push({ name, error: { code: e.code, message: String(e.message).replace(/projects\/[^ "]*/g, '<name>') } }); }
}
const sorted = (v) => Object.fromEntries(Object.keys(v).sort().map((k) => [k, v[k]]));
const counted = async (q) => (await getCountFromServer(q)).data();
const aggregated = async (q, spec) => sorted((await getAggregateFromServer(q, spec)).data());

const orders = collection(db, 'orders');
await step('seed', async () => {
  const batch = writeBatch(db);
  const rows = {
    // Integers, a double, a string and a missing `total`.
    orders: {
      a: { total: 10, qty: 1, status: 'paid' }, b: { total: 2.5, qty: 3, status: 'paid' },
      c: { total: 7, qty: 2, status: 'open' }, d: { total: 'n/a', qty: 1, status: 'open' },
      e: { qty: 5, status: 'void' },
    },
    ints: { a: { v: 1 }, b: { v: 2 }, c: { v: 3 } },
    doubles: { a: { v: 0.5 }, b: { v: 1.25 }, c: { v: 2.25 } },
    // Numbers next to every other kind of value, and a document without `v`.
    mixed: {
      int: { v: 1 }, double: { v: 2.5 }, string: { v: 'x' }, null: { v: null }, bool: { v: true },
      array: { v: [1] }, map: { v: { n: 1 } }, missing: { w: 1 },
    },
    nan: { a: { v: NaN }, b: { v: 1 } },
    infinity: { a: { v: Infinity }, b: { v: 1 } },
    big: { x: { n: Number.MAX_SAFE_INTEGER }, y: { n: Number.MAX_SAFE_INTEGER } },
  };
  for (const [coll, docs] of Object.entries(rows)) {
    for (const [id, row] of Object.entries(docs)) batch.set(doc(db, coll, id), row);
  }
  batch.set(doc(db, 'orders/a/lines/1'), { qty: 4, price: 1.5 });
  batch.set(doc(db, 'orders/b/lines/1'), { qty: 6, price: 2 });
  batch.set(doc(db, 'returns/r/lines/1'), { qty: 1, price: 'free' });
  await batch.commit();
  return 'ok';
});

await step('count of a collection', () => counted(orders));
await step('count with a filter', () => counted(query(orders, where('status', '==', 'open'))));
await step('count with a limit', () => counted(query(orders, limit(2))));
await step('count with a limit above the number of documents', () => counted(query(orders, limit(10))));
await step('count with orderBy (documents without the field left out)', () => counted(query(orders, orderBy('total'))));
await step('count with limitToLast', () => counted(query(orders, orderBy('qty'), limitToLast(2))));
await step('count with a cursor', () => counted(query(orders, orderBy('qty'), startAfter(1))));
await step('count with an or filter', () => counted(query(orders, or(where('status', '==', 'open'), where('qty', '>', 4)))));
await step('count with an and of an inequality', () => counted(query(orders, and(where('qty', '>=', 2), where('status', '!=', 'void')))));
await step('count of an empty collection', () => counted(collection(db, 'nothing')));
await step('count after a write', async () => {
  await setDoc(doc(db, 'orders/f'), { total: 1, qty: 1, status: 'paid' });
  const after = await counted(orders);
  await deleteDoc(doc(db, 'orders/f'));
  return { after, deleted: await counted(orders) };
});

await step('sum and average of integers', () => aggregated(collection(db, 'ints'), { total: sum('v'), mean: average('v') }));
await step('sum and average of doubles', () => aggregated(collection(db, 'doubles'), { total: sum('v'), mean: average('v') }));
await step('sum and average of integers and a double', () => aggregated(orders, { total: sum('total'), mean: average('total') }));
await step('sum and average of mixed values (non-numbers and a missing field left out)', () => aggregated(collection(db, 'mixed'), { total: sum('v'), mean: average('v') }));
await step('sum and average with NaN', () => aggregated(collection(db, 'nan'), { total: sum('v'), mean: average('v') }));
await step('sum and average with Infinity', () => aggregated(collection(db, 'infinity'), { total: sum('v'), mean: average('v') }));
await step('sum beyond 2^53', () => aggregated(collection(db, 'big'), { total: sum('n'), mean: average('n') }));
await step('sum and average of a field no document has', () => aggregated(collection(db, 'ints'), { total: sum('nope'), mean: average('nope') }));
await step('average of nothing (an empty result)', () => aggregated(query(orders, where('status', '==', 'none')), { n: count(), total: sum('total'), mean: average('total') }));
await step('sum and average with a filter and a limit', () => aggregated(query(orders, where('status', '==', 'paid'), orderBy('total'), limit(1)), { total: sum('total'), mean: average('total') }));

await step('count, sum and average in one getAggregateFromServer', () => aggregated(orders, { n: count(), qty: sum('qty'), meanQty: average('qty') }));
await step('count next to sum and average of a sparse field', () => aggregated(orders, { n: count(), total: sum('total'), mean: average('total') }));
await step('five aggregations in one getAggregateFromServer', () => aggregated(orders, {
  n: count(), qty: sum('qty'), meanQty: average('qty'), total: sum('total'), mean: average('total'),
}));
await step('collection group count, sum and average', () => aggregated(collectionGroup(db, 'lines'), { n: count(), qty: sum('qty'), price: average('price') }));
await step('collection group count with a filter', () => counted(query(collectionGroup(db, 'lines'), where('qty', '>', 3))));
await step('count and sum of a subcollection', () => aggregated(collection(db, 'orders/a/lines'), { n: count(), qty: sum('qty') }));

await step('six aggregations (refused)', () => aggregated(orders, {
  a: count(), b: count(), c: count(), d: count(), e: count(), f: count(),
}));
await step('sum of documentId() (refused)', () => aggregated(orders, { total: sum(documentId()) }));
await step('a count after the refused aggregations', () => counted(orders));

console.log(JSON.stringify(out, (k, v) => (typeof v === 'number' && !Number.isFinite(v) ? String(v) : v), 1));
await terminate(db);
process.exit(0);
