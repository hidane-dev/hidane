// Drives the REST surface through the web SDK's Lite build (`firebase/firestore/lite`, which
// talks REST over fetch) and prints a transcript (keys sorted, #108), so the transcripts of two
// emulators can be diffed. FIRESTORE_EMULATOR_HOST selects the emulator; start it fresh.
//
// The Admin SDK's `preferRest: true` is left out: it looks for Google credentials even with
// FIRESTORE_EMULATOR_HOST set and fails the same way against both emulators.
import { initializeApp } from 'firebase/app';
import {
  collection, connectFirestoreEmulator, deleteDoc, doc, getAggregate, getCount, getDoc,
  getDocs, getFirestore, increment, limit, orderBy, query, runTransaction, setDoc, sum,
  terminate, updateDoc, where, writeBatch,
} from 'firebase/firestore/lite';

const [host, port] = process.env.FIRESTORE_EMULATOR_HOST.split(':');
const sorted = (v) => (v && typeof v === 'object' && !Array.isArray(v)
  ? Object.fromEntries(Object.keys(v).sort().map((k) => [k, sorted(v[k])])) : v);
const out = [];
async function step(name, fn) {
  try { out.push({ name, ok: sorted(await fn()) }); }
  catch (e) { out.push({ name, error: { code: e.code } }); }
}

const app = initializeApp({ projectId: 'demo-sdk-rest', apiKey: 'demo' });
const lite = getFirestore(app);
connectFirestoreEmulator(lite, host, Number(port));
const c = collection(lite, 'lite');
const data = (s) => (s.exists() ? s.data() : null);

await step('lite: setDoc', async () => { await setDoc(doc(c, 'a'), { n: 1, s: 'x', tags: ['a'], m: { k: true } }); return data(await getDoc(doc(c, 'a'))); });
await step('lite: updateDoc with increment', async () => { await updateDoc(doc(c, 'a'), { n: increment(2) }); return data(await getDoc(doc(c, 'a'))); });
await step('lite: getDoc missing', async () => data(await getDoc(doc(c, 'missing'))));
await step('lite: writeBatch', async () => {
  const b = writeBatch(lite);
  b.set(doc(c, 'b'), { n: 5 }); b.set(doc(c, 'c'), { n: 9 }); b.delete(doc(c, 'zz'));
  await b.commit();
  return (await getDocs(query(c, orderBy('n')))).docs.map((d) => [d.id, d.data().n]);
});
await step('lite: query', async () => (await getDocs(query(c, where('n', '>', 2), orderBy('n', 'desc'), limit(2)))).docs.map((d) => d.id));
await step('lite: getCount', async () => (await getCount(c)).data());
await step('lite: getAggregate', async () => (await getAggregate(c, { total: sum('n') })).data());
await step('lite: runTransaction', async () => runTransaction(lite, async (tx) => {
  const s = await tx.get(doc(c, 'b'));
  tx.update(doc(c, 'b'), { n: s.data().n + 1 });
  return s.data().n;
}));
await step('lite: deleteDoc', async () => { await deleteDoc(doc(c, 'c')); return data(await getDoc(doc(c, 'c'))); });
await step('lite: update a missing document', async () => { await updateDoc(doc(c, 'nobody'), { n: 1 }); return 'unexpected'; });

console.log(JSON.stringify(out, null, 1));
await terminate(lite);
process.exit(0);
