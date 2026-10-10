// Browser scenarios for comparing emulators over WebChannel (results/browser-webchannel-*.json).
//
// Bundle it from a directory where `npm i firebase esbuild` was run (outside the repository):
//   npx esbuild page.js --bundle --format=iife --outfile=bundle.js
// serve `bundle.js` from a page on another origin (CORS is part of what is tested), and call
// `scenario({host, port, settings, mock, steps})` and `badWrite({host, port})` from the page,
// with Playwright or the browser console, once per emulator. `idle({host, port, seconds})`
// keeps a listener idle to watch keep-alives through tap.py.
import { initializeApp } from 'firebase/app';
import {
  connectFirestoreEmulator, deleteDoc, doc, getDoc, initializeFirestore, onSnapshot, setDoc,
  terminate, updateDoc, collection, query, where, getDocs, runTransaction, writeBatch,
} from 'firebase/firestore';

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let n = 0;

// One scenario: its own app, so its own WebChannel sessions.
window.scenario = async ({ host, port, settings = {}, mock, steps = 'basic' }) => {
  const app = initializeApp({ projectId: 'demo-wc', apiKey: 'demo' }, `app${n++}`);
  const db = initializeFirestore(app, settings);
  connectFirestoreEmulator(db, host, port, mock ? { mockUserToken: mock } : undefined);
  const log = [];
  const ref = doc(db, `c/${steps}-${n}`);
  const unsubscribe = onSnapshot(ref, { includeMetadataChanges: true }, (s) =>
    log.push({ exists: s.exists(), data: s.data() ?? null, fromCache: s.metadata.fromCache, pending: s.metadata.hasPendingWrites }));
  await sleep(500);
  await setDoc(ref, { n: 1 });
  await sleep(300);
  await updateDoc(ref, { n: 2 });
  await sleep(300);
  log.push({ get: (await getDoc(ref)).data() });
  if (steps === 'more') {
    const q = query(collection(db, 'c'), where('n', '>=', 1));
    const unsub2 = onSnapshot(q, (s) => log.push({ query: s.docs.map((d) => d.id) }));
    await sleep(300);
    const b = writeBatch(db);
    b.set(doc(db, 'c/b1'), { n: 5 }); b.set(doc(db, 'c/b2'), { n: 6 });
    await b.commit();
    await sleep(300);
    unsub2();
  }
  await deleteDoc(ref);
  await sleep(500);
  unsubscribe();
  await sleep(300);
  await terminate(db);
  return log;
};

// A listener left idle for `seconds`, to see keep-alives and back-channel lifetimes.
window.idle = async ({ host, port, seconds, settings = {}, mock }) => {
  const app = initializeApp({ projectId: 'demo-wc', apiKey: 'demo' }, `idle${n++}`);
  const db = initializeFirestore(app, settings);
  connectFirestoreEmulator(db, host, port, mock ? { mockUserToken: mock } : undefined);
  const log = [];
  const unsubscribe = onSnapshot(doc(db, 'c/idle'), (s) => log.push({ t: Math.round(performance.now()), exists: s.exists(), data: s.data() ?? null }));
  await sleep(seconds * 1000);
  await setDoc(doc(db, 'c/idle'), { n: seconds });
  await sleep(1000);
  unsubscribe();
  await terminate(db);
  return log;
};

// A write the server refuses (a value over 1,048,487 bytes), then a good one.
window.badWrite = async ({ host, port, settings = {} }) => {
  const app = initializeApp({ projectId: 'demo-wc', apiKey: 'demo' }, `bad${n++}`);
  const db = initializeFirestore(app, settings);
  connectFirestoreEmulator(db, host, port);
  const out = [];
  try { await setDoc(doc(db, 'c/big'), { s: 'x'.repeat(1048488) }); out.push('ok'); } catch (e) { out.push(`${e.code}: ${e.message}`); }
  try { await setDoc(doc(db, 'c/good'), { n: 1 }); out.push('ok'); } catch (e) { out.push(`${e.code}: ${e.message}`); }
  await terminate(db);
  return out;
};
