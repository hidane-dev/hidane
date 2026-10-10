// Runs transactions through @google-cloud/firestore (the engine of firebase-admin) and prints
// a transcript without timestamps, so the transcripts of two emulators can be diffed.
// FIRESTORE_EMULATOR_HOST selects the emulator; start it fresh (or POST /reset) first.
//
// The SDK starts a transaction lazily: its first read carries `newTransaction`, a commit that
// fails with ABORTED is retried with backoff, and a failing callback rolls back.
import { Firestore, Timestamp } from '@google-cloud/firestore';

const db = new Firestore({ projectId: 'demo-sdk-txn' });
const out = [];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function step(name, fn) {
  try { out.push({ name, ok: await fn() }); }
  catch (e) { out.push({ name, error: { code: e.code, message: String(e.details ?? e.message).replace(/projects\/[^ ]*/g, '<name>') } }); }
}
/** Runs `fn` in a transaction and counts how many attempts it took. */
async function attempts(fn, options) {
  let n = 0;
  const result = await db.runTransaction(async (tx) => { n += 1; return fn(tx, n); }, options);
  return { attempts: n, result };
}

const c = db.collection('txn');
await step('create when missing', async () => {
  const r = await attempts(async (tx) => {
    const s = await tx.get(c.doc('a'));
    if (!s.exists) tx.set(c.doc('a'), { n: 0 });
    return s.exists;
  });
  return { ...r, data: (await c.doc('a').get()).data() };
});
await step('read-modify-write', async () => {
  const r = await attempts(async (tx) => {
    const n = (await tx.get(c.doc('a'))).get('n');
    tx.update(c.doc('a'), { n: n + 1 });
    return n;
  });
  return { ...r, data: (await c.doc('a').get()).data() };
});
await step('getAll', async () => attempts(async (tx) =>
  (await tx.getAll(c.doc('a'), c.doc('missing'))).map((s) => ({ id: s.id, exists: s.exists }))));
await step('write-only transaction', async () => {
  const r = await attempts(async (tx) => { tx.set(c.doc('b'), { n: 1 }); tx.delete(c.doc('nothing')); });
  return { ...r, data: (await c.doc('b').get()).data() };
});
let snapshotTime;
await step('read-only', async () => {
  snapshotTime = (await c.doc('a').get()).readTime;
  return attempts(async (tx) => (await tx.get(c.doc('a'))).data(), { readOnly: true });
});
await step('read-only at a past read time', async () => {
  await c.doc('a').update({ n: 100 });
  return attempts(async (tx) => (await tx.get(c.doc('a'))).data(), { readOnly: true, readTime: snapshotTime });
});
await step('read-only cannot write', async () => attempts(async (tx) => { tx.set(c.doc('ro'), { n: 1 }); }, { readOnly: true }));
await step('a failing callback rolls back and releases its locks', async () => {
  let failed;
  try {
    await db.runTransaction(async (tx) => { await tx.get(c.doc('a')); throw new Error('boom'); });
  } catch (e) { failed = e.message; }
  const start = Date.now();
  await c.doc('a').set({ n: 1 });
  return { failed, outsideWriteWaited: Date.now() - start > 1000 };
});
await step('create of an existing document', async () => {
  try {
    await attempts(async (tx) => { await tx.get(c.doc('b')); tx.create(c.doc('b'), { n: 2 }); });
    return 'unexpected';
  } catch (e) { return { code: e.code }; }
});
await step('an outside write waits for the transaction that read the document', async () => {
  const order = [];
  let release;
  const reading = new Promise((r) => { release = r; });
  const txn = db.runTransaction(async (tx) => {
    await tx.get(c.doc('w'));
    release();
    await sleep(500);
    tx.set(c.doc('w'), { by: 'transaction' });
  }).then(() => order.push('transaction'));
  await reading;
  const outside = c.doc('w').set({ by: 'outside' }).then(() => order.push('outside'), (e) => order.push(`outside failed ${e.code}`));
  await Promise.all([txn, outside]);
  return { order, final: (await c.doc('w').get()).data() };
});
await step('an outside write gives up after the lock timeout', async () => {
  let release;
  const reading = new Promise((r) => { release = r; });
  const txn = db.runTransaction(async (tx) => {
    await tx.get(c.doc('t'));
    release();
    await sleep(3000);
    tx.set(c.doc('t'), { by: 'transaction' });
  });
  await reading;
  const outside = await c.doc('t').set({ by: 'outside' }).then(() => 'ok', (e) => ({ code: e.code, message: e.details }));
  await txn;
  return { outside, final: (await c.doc('t').get()).data() };
});
await step('two transactions increment the same counter', async () => {
  await c.doc('counter').set({ n: 0 });
  const inc = () => attempts(async (tx) => {
    const n = (await tx.get(c.doc('counter'))).get('n');
    await sleep(100);
    tx.update(c.doc('counter'), { n: n + 1 });
  });
  const runs = await Promise.all([inc(), inc()]);
  return { totalAttempts: runs.reduce((a, r) => a + r.attempts, 0), final: (await c.doc('counter').get()).data() };
});

console.log(JSON.stringify(out, null, 1));
await db.terminate();
