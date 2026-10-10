// Listens through both the Admin SDK (@google-cloud/firestore) and the web SDK (`firebase`, in
// Node), writes through the Admin SDK, and prints every snapshot each listener received, so the
// transcripts of two emulators can be diffed. FIRESTORE_EMULATOR_HOST selects the emulator;
// start it fresh (or POST /reset) first.
//
// Snapshots record document IDs, data (keys sorted, #108) and the change list. After each
// write the script waits until every listener is quiet for 300 ms.
import { FieldValue, Firestore } from '@google-cloud/firestore';
import { initializeApp } from 'firebase/app';
import {
  collection, connectFirestoreEmulator, doc, getDoc, getDocs, getFirestore, limit, onSnapshot,
  orderBy, query, terminate, where,
} from 'firebase/firestore';

const [host, port] = process.env.FIRESTORE_EMULATOR_HOST.split(':');
const admin = new Firestore({ projectId: 'demo-sdk-listen' });
const app = initializeApp({ projectId: 'demo-sdk-listen', apiKey: 'demo' });
const web = getFirestore(app);
connectFirestoreEmulator(web, host, Number(port));

const sorted = (v) => (v && typeof v === 'object' && !Array.isArray(v)
  ? Object.fromEntries(Object.keys(v).sort().map((k) => [k, sorted(v[k])])) : v);
const log = [];
let lastEvent = Date.now();
function record(listener, entry) { log.push({ listener, ...entry }); lastEvent = Date.now(); }
const quiet = async () => {
  await new Promise((r) => setTimeout(r, 150));
  while (Date.now() - lastEvent < 300) await new Promise((r) => setTimeout(r, 50));
};
async function step(name, fn) { log.push({ step: name }); await fn(); await quiet(); }

const c = admin.collection('cities');
const adminQuery = (name, q) => q.onSnapshot(
  (s) => record(name, { docs: s.docs.map((d) => [d.id, sorted(d.data())]), changes: s.docChanges().map((x) => [x.type, x.doc.id, x.oldIndex, x.newIndex]) }),
  (e) => record(name, { error: e.code }));
const webQuery = (name, q) => onSnapshot(q,
  (s) => record(name, { docs: s.docs.map((d) => [d.id, sorted(d.data())]), changes: s.docChanges().map((x) => [x.type, x.doc.id, x.oldIndex, x.newIndex]) }),
  (e) => record(name, { error: e.code }));

await step('seed', async () => {
  const b = admin.batch();
  b.set(c.doc('SF'), { pop: 860, state: 'CA' });
  b.set(c.doc('LA'), { pop: 3900, state: 'CA' });
  b.set(c.doc('TOK'), { pop: 9000, state: null });
  await b.commit();
});
const unsubscribe = [];
await step('attach listeners', async () => {
  unsubscribe.push(adminQuery('admin: all', c));
  unsubscribe.push(adminQuery('admin: pop > 1000', c.where('pop', '>', 1000)));
  unsubscribe.push(adminQuery('admin: top 2 by pop', c.orderBy('pop', 'desc').limit(2)));
  unsubscribe.push(admin.doc('cities/SF').onSnapshot((s) => record('admin: doc SF', { exists: s.exists, data: sorted(s.data() ?? null) })));
  unsubscribe.push(admin.doc('cities/NY').onSnapshot((s) => record('admin: doc NY', { exists: s.exists, data: sorted(s.data() ?? null) })));
  unsubscribe.push(adminQuery('admin: group landmarks', admin.collectionGroup('landmarks')));
  unsubscribe.push(webQuery('web: CA by pop', query(collection(web, 'cities'), where('state', '==', 'CA'), orderBy('pop'))));
  unsubscribe.push(webQuery('web: top 2 by pop', query(collection(web, 'cities'), orderBy('pop', 'desc'), limit(2))));
  unsubscribe.push(onSnapshot(doc(web, 'cities/NY'), (s) => record('web: doc NY', { exists: s.exists(), data: sorted(s.data() ?? null) })));
});
await step('add NY (enters several queries)', () => c.doc('NY').set({ pop: 8300, state: 'NY' }));
await step('update SF (stays in all)', () => c.doc('SF').update({ pop: 870 }));
await step('update LA below 1000 (leaves pop > 1000, moves in order)', () => c.doc('LA').update({ pop: 999 }));
await step('update TOK above all (changes the top 2)', () => c.doc('TOK').update({ pop: 9500 }));
await step('no-op write', () => c.doc('SF').set({ pop: 870, state: 'CA' }));
await step('write to another collection', () => admin.doc('other/x').set({ n: 1 }));
await step('batch: delete TOK, add BJ, change NY state', async () => {
  const b = admin.batch();
  b.delete(c.doc('TOK')); b.set(c.doc('BJ'), { pop: 21500, state: null }); b.update(c.doc('NY'), { state: 'CA' });
  await b.commit();
});
await step('add a landmark', () => admin.doc('cities/SF/landmarks/gg').set({ name: 'Golden Gate' }));
await step('server timestamp', () => c.doc('SF').update({ at: FieldValue.serverTimestamp() }).then(() => {}));
await step('delete NY', () => c.doc('NY').delete());
await step('web getDoc and getDocs', async () => {
  const one = await getDoc(doc(web, 'cities/SF'));
  const all = await getDocs(query(collection(web, 'cities'), orderBy('pop')));
  record('web: get', { one: one.exists(), all: all.docs.map((d) => d.id) });
});
await step('detach', async () => { for (const u of unsubscribe) u(); });

// Timestamps differ between runs; keep their presence only. Listeners fire in no particular
// order, so within each step the snapshots are grouped by listener (each listener's own order
// is kept).
const isTimestamp = (v) => v && typeof v === 'object' && ('_seconds' in v || ('seconds' in v && 'nanoseconds' in v));
const clean = JSON.parse(JSON.stringify(log, (k, v) => (isTimestamp(v) ? 'timestamp' : v)));
const steps = [];
for (const entry of clean) {
  if (entry.step) steps.push({ step: entry.step, listeners: {} });
  else (steps.at(-1).listeners[entry.listener] ??= []).push(Object.fromEntries(Object.entries(entry).filter(([k]) => k !== 'listener')));
}
for (const s of steps) s.listeners = Object.fromEntries(Object.keys(s.listeners).sort().map((k) => [k, s.listeners[k]]));
console.log(JSON.stringify(steps, null, 1));
await terminate(web);
await admin.terminate();
process.exit(0);
