// Resumes listeners through the web SDK (`firebase`, in Node): disableNetwork / enableNetwork
// makes it listen again with its resume tokens. Writes go through the Admin SDK while it is
// offline. Prints every snapshot, so the transcripts of two emulators can be diffed.
// Two limit queries (top 2 by n, #146) see their window move while offline: a new document
// pushes `w/b` out, and deleting `v/c` lets `v/a` in; neither `w/b` nor `v/a` changes itself.
// FIRESTORE_EMULATOR_HOST selects the emulator; start it fresh (or POST /reset) first.
import { Firestore } from '@google-cloud/firestore';
import { initializeApp } from 'firebase/app';
import {
  collection, connectFirestoreEmulator, disableNetwork, doc, enableNetwork, getFirestore,
  limit, onSnapshot, orderBy, query, terminate, where,
} from 'firebase/firestore';

const [host, port] = process.env.FIRESTORE_EMULATOR_HOST.split(':');
const admin = new Firestore({ projectId: 'demo-sdk-resume' });
const app = initializeApp({ projectId: 'demo-sdk-resume', apiKey: 'demo' });
const web = getFirestore(app);
connectFirestoreEmulator(web, host, Number(port));

const sorted = (v) => (v && typeof v === 'object' && !Array.isArray(v)
  ? Object.fromEntries(Object.keys(v).sort().map((k) => [k, sorted(v[k])])) : v);
const steps = [];
let lastEvent = Date.now();
const record = (listener, entry) => { (steps.at(-1).listeners[listener] ??= []).push(entry); lastEvent = Date.now(); };
const quiet = async () => {
  await new Promise((r) => setTimeout(r, 200));
  while (Date.now() - lastEvent < 400) await new Promise((r) => setTimeout(r, 50));
};
async function step(name, fn) { steps.push({ step: name, listeners: {} }); await fn(); await quiet(); }

const c = admin.collection('c');
const w = admin.collection('w');
const v = admin.collection('v');
await step('seed', async () => {
  const b = admin.batch();
  for (const [id, n] of [['a', 1], ['b', 2], ['e', 3], ['same', 4]]) b.set(c.doc(id), { n });
  for (const top of [w, v]) for (const [id, n] of [['a', 1], ['b', 2], ['c', 3]]) b.set(top.doc(id), { n });
  await b.commit();
});
const snapshot = (s) => ({ docs: s.docs.map((d) => [d.id, sorted(d.data())]), changes: s.docChanges().map((x) => [x.type, x.doc.id]) });
// Resolves on the listener's first snapshot from the server.
const listen = (name, q) => new Promise((ready) => {
  onSnapshot(q, (s) => {
    record(name, snapshot(s));
    if (!s.metadata.fromCache) ready();
  });
});
await step('listen', async () => {
  // Wait until the queries have their first snapshots from the server (the document listener
  // may be served from the cache they filled), then give the document target time to become
  // current, so going offline later resumes them all.
  await listen('n < 10', query(collection(web, 'c'), where('n', '<', 10)));
  for (const top of ['w', 'v']) await listen(`${top}: top 2`, query(collection(web, top), orderBy('n', 'desc'), limit(2)));
  onSnapshot(doc(web, 'c/b'), (s) => record('doc b', { exists: s.exists(), data: sorted(s.data() ?? null) }));
  await new Promise((r) => setTimeout(r, 1000));
});
await step('go offline', () => disableNetwork(web));
await step('write while offline', async () => {
  await c.doc('a').update({ n: 5 });
  await c.doc('b').delete();
  await c.doc('d').set({ n: 6 });
  await c.doc('e').update({ n: 50 });
  await w.doc('d').set({ n: 9 });
  await v.doc('c').delete();
});
await step('back online', () => enableNetwork(web));
await step('write while online', () => c.doc('b').set({ n: 7 }));
await step('offline again, nothing changes, online again', async () => {
  await disableNetwork(web);
  await new Promise((r) => setTimeout(r, 300));
  await enableNetwork(web);
});

// Listeners fire in no particular order: list them by name.
for (const s of steps) s.listeners = Object.fromEntries(Object.keys(s.listeners).sort().map((k) => [k, s.listeners[k]]));
console.log(JSON.stringify(steps, null, 1));
await terminate(web);
await admin.terminate();
process.exit(0);
