// Clears data the ways test suites and the Emulator UI do, with listeners attached, and prints
// what each listener saw, so the transcripts of two emulators can be diffed.
// FIRESTORE_EMULATOR_HOST selects the emulator; start it fresh (or POST /reset) first.
//
// Steps: the Emulator UI's recursive delete (DELETE …/documents/{path}), rules-unit-testing's
// clearFirestore() (DELETE …/documents), and POST /reset.
import { Firestore } from '@google-cloud/firestore';
import { initializeTestEnvironment } from '@firebase/rules-unit-testing';
import { initializeApp } from 'firebase/app';
import { collection, connectFirestoreEmulator, doc, getFirestore, onSnapshot, terminate } from 'firebase/firestore';

const [host, port] = process.env.FIRESTORE_EMULATOR_HOST.split(':');
const projectId = 'demo-sdk-clear';
const admin = new Firestore({ projectId });
const app = initializeApp({ projectId, apiKey: 'demo' });
const web = getFirestore(app);
connectFirestoreEmulator(web, host, Number(port));
const testEnv = await initializeTestEnvironment({ projectId, firestore: { host, port: Number(port) } });

const steps = [];
let lastEvent = Date.now();
const record = (listener, entry) => { (steps.at(-1).listeners[listener] ??= []).push(entry); lastEvent = Date.now(); };
const quiet = async () => {
  await new Promise((r) => setTimeout(r, 300));
  while (Date.now() - lastEvent < 500) await new Promise((r) => setTimeout(r, 50));
};
async function step(name, fn) { steps.push({ step: name, listeners: {} }); await fn(); await quiet(); }
const seed = async () => {
  const b = admin.batch();
  for (const p of ['c/a', 'c/b', 'c/a/sub/x', 'd/z']) b.set(admin.doc(p), { n: 1 });
  await b.commit();
};

await step('seed', seed);
const unsubscribe = [];
await step('listen', async () => {
  unsubscribe.push(onSnapshot(collection(web, 'c'), (s) => record('web: c', { ids: s.docs.map((d) => d.id), changes: s.docChanges().map((x) => [x.type, x.doc.id]) })));
  unsubscribe.push(onSnapshot(doc(web, 'c/a'), (s) => record('web: c/a', { exists: s.exists() })));
  unsubscribe.push(admin.collectionGroup('sub').onSnapshot((s) => record('admin: group sub', { ids: s.docs.map((d) => d.ref.path) })));
  unsubscribe.push(admin.collection('d').onSnapshot((s) => record('admin: d', { ids: s.docs.map((d) => d.id) })));
  await new Promise((r) => setTimeout(r, 1500));
});
await step('Emulator UI: delete c/a recursively', async () => {
  await fetch(`http://${host}:${port}/emulator/v1/projects/${projectId}/databases/(default)/documents/c/a`, { method: 'DELETE' });
});
await step('seed again', seed);
await step('rules-unit-testing: clearFirestore()', () => testEnv.clearFirestore());
await step('seed again', seed);
await step('POST /reset', async () => { await fetch(`http://${host}:${port}/reset`, { method: 'POST' }); });
await step('write after the reset', () => admin.doc('d/after').set({ n: 1 }));

// Listeners fire in no particular order: list them by name.
for (const s of steps) s.listeners = Object.fromEntries(Object.keys(s.listeners).sort().map((k) => [k, s.listeners[k]]));
console.log(JSON.stringify(steps, null, 1));
for (const u of unsubscribe) u();
await testEnv.cleanup();
await terminate(web);
await admin.terminate();
process.exit(0);
