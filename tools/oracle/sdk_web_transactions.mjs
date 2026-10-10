// Runs transactions through the firebase-js-sdk (`firebase/firestore`, the web SDK, here in
// Node) and prints a transcript, so the transcripts of two emulators can be diffed.
// FIRESTORE_EMULATOR_HOST selects the emulator.
//
// The web SDK never opens a server transaction: it reads with BatchGetDocuments and commits
// with update-time / exists preconditions, retrying on FAILED_PRECONDITION. Only transactions
// are used here, because getDoc / setDoc go through the Listen and Write streams (#18, #19).
import { initializeApp } from 'firebase/app';
import { connectFirestoreEmulator, doc, getFirestore, runTransaction, terminate } from 'firebase/firestore';

const [host, port] = process.env.FIRESTORE_EMULATOR_HOST.split(':');
const app = initializeApp({ projectId: 'demo-web-txn', apiKey: 'demo' });
const db = getFirestore(app);
connectFirestoreEmulator(db, host, Number(port));

const out = [];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function step(name, fn) {
  try { out.push({ name, ok: await fn() }); }
  catch (e) { out.push({ name, error: { code: e.code, message: e.message.replace(/projects\/[^ ]*/g, '<name>') } }); }
}
async function attempts(fn) {
  let n = 0;
  const result = await runTransaction(db, async (tx) => { n += 1; return fn(tx, n); });
  return { attempts: n, result };
}
const read = (path) => attempts(async (tx) => (await tx.get(doc(db, path))).data() ?? null);

await step('create when missing', async () => attempts(async (tx) => {
  const s = await tx.get(doc(db, 'txn/a'));
  if (!s.exists()) tx.set(doc(db, 'txn/a'), { n: 0 });
  return s.exists();
}));
await step('read-modify-write', async () => {
  const r = await attempts(async (tx) => {
    const n = (await tx.get(doc(db, 'txn/a'))).get('n');
    tx.update(doc(db, 'txn/a'), { n: n + 1 });
    return n;
  });
  return { ...r, after: (await read('txn/a')).result };
});
await step('blind writes', async () => {
  const r = await attempts(async (tx) => { tx.set(doc(db, 'txn/b'), { n: 1 }); tx.delete(doc(db, 'txn/nothing')); });
  return { ...r, after: (await read('txn/b')).result };
});
await step('two transactions increment the same counter', async () => {
  await attempts(async (tx) => { tx.set(doc(db, 'txn/counter'), { n: 0 }); });
  const inc = () => attempts(async (tx) => {
    const n = (await tx.get(doc(db, 'txn/counter'))).get('n');
    await sleep(100);
    tx.update(doc(db, 'txn/counter'), { n: n + 1 });
  });
  const runs = await Promise.all([inc(), inc()]);
  return { totalAttempts: runs.reduce((a, r) => a + r.attempts, 0), after: (await read('txn/counter')).result };
});
await step('two transactions create the same document', async () => {
  const create = (by) => attempts(async (tx) => {
    const s = await tx.get(doc(db, 'txn/once'));
    await sleep(100);
    if (!s.exists()) tx.set(doc(db, 'txn/once'), { by });
    return s.exists();
  });
  const runs = await Promise.all([create('first'), create('second')]);
  return { totalAttempts: runs.reduce((a, r) => a + r.attempts, 0), sawExisting: runs.filter((r) => r.result).length };
});
await step('a failing callback', async () => {
  try { await runTransaction(db, async (tx) => { await tx.get(doc(db, 'txn/a')); throw new Error('boom'); }); return 'unexpected'; }
  catch (e) { return e.message; }
});

console.log(JSON.stringify(out, null, 1));
await terminate(db);
process.exit(0);
