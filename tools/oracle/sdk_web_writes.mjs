// Writes through the firebase-js-sdk (`firebase/firestore`, the web SDK, here in Node), which
// sends them over the Write stream, and prints a transcript, so the transcripts of two
// emulators can be diffed. FIRESTORE_EMULATOR_HOST selects the emulator.
//
// Documents are read back inside transactions (BatchGetDocuments): getDoc would use the Listen
// stream (#18). Keys are printed sorted: the official emulator returns fields in the order they
// were written, hidane in name order (#108).
import { initializeApp } from 'firebase/app';
import {
  arrayUnion, connectFirestoreEmulator, deleteDoc, deleteField, doc, getFirestore, increment,
  runTransaction, serverTimestamp, setDoc, terminate, updateDoc, writeBatch,
} from 'firebase/firestore';

const [host, port] = process.env.FIRESTORE_EMULATOR_HOST.split(':');
const app = initializeApp({ projectId: 'demo-web-writes', apiKey: 'demo' });
const db = getFirestore(app);
connectFirestoreEmulator(db, host, Number(port));

const out = [];
async function step(name, fn) {
  try { out.push({ name, ok: await fn() }); }
  catch (e) { out.push({ name, error: { code: e.code } }); }
}
const sorted = (v) => (v && typeof v === 'object' && !Array.isArray(v)
  ? Object.fromEntries(Object.keys(v).sort().map((k) => [k, sorted(v[k])])) : v);
const read = (path) => runTransaction(db, async (tx) => {
  const s = await tx.get(doc(db, path));
  if (!s.exists()) return null;
  const data = s.data();
  for (const [k, v] of Object.entries(data)) if (v && typeof v.toMillis === 'function') data[k] = 'timestamp';
  return sorted(data);
});

await step('setDoc', async () => { await setDoc(doc(db, 'w/a'), { n: 1, s: 'x', tags: ['a'] }); return read('w/a'); });
await step('setDoc merge', async () => { await setDoc(doc(db, 'w/a'), { m: { x: 1 } }, { merge: true }); return read('w/a'); });
await step('updateDoc with transforms', async () => {
  await updateDoc(doc(db, 'w/a'), { n: increment(2), tags: arrayUnion('b'), at: serverTimestamp(), s: deleteField() });
  return read('w/a');
});
await step('updateDoc of a missing document', async () => { await updateDoc(doc(db, 'w/missing'), { n: 1 }); return 'unexpected'; });
await step('deleteDoc', async () => { await deleteDoc(doc(db, 'w/a')); return read('w/a'); });
await step('writeBatch', async () => {
  const b = writeBatch(db);
  b.set(doc(db, 'w/b1'), { n: 1 }); b.set(doc(db, 'w/b2'), { n: 2 }); b.delete(doc(db, 'w/b1'));
  await b.commit();
  return { b1: await read('w/b1'), b2: await read('w/b2') };
});
await step('fifty writes at once', async () => {
  await Promise.all(Array.from({ length: 50 }, (_, i) => setDoc(doc(db, `burst/d${i}`), { i })));
  return { d0: await read('burst/d0'), d49: await read('burst/d49') };
});
await step('fifty increments of one document', async () => {
  await setDoc(doc(db, 'w/counter'), { n: 0 });
  await Promise.all(Array.from({ length: 50 }, () => updateDoc(doc(db, 'w/counter'), { n: increment(1) })));
  return read('w/counter');
});
await step('a write after a failed one', async () => { await setDoc(doc(db, 'w/after'), { ok: true }); return read('w/after'); });

console.log(JSON.stringify(out, null, 1));
await terminate(db);
process.exit(0);
