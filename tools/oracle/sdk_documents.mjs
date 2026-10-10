// Runs the same document operations through @google-cloud/firestore (the engine of
// firebase-admin) and prints a transcript without absolute timestamps, so the transcripts of
// two emulators can be diffed. FIRESTORE_EMULATOR_HOST selects the emulator.
import { Firestore, FieldValue } from '@google-cloud/firestore';

const db = new Firestore({ projectId: 'demo-sdk-diff' });
const out = [];
const t = (ts) => ts && `${ts.seconds}.${String(ts.nanoseconds).padStart(9, '0')}`;
async function step(name, fn) {
  try { out.push({ name, ok: await fn() }); }
  catch (e) { out.push({ name, error: { code: e.code, message: String(e.details ?? e.message).replace(/\d{10,}/g, '<n>') } }); }
}

const users = db.collection('users');
let first;
await step('set alice', async () => { first = await users.doc('alice').set({ name: 'Alice', age: 30, tags: ['a', 'b'], nested: { x: 1 } }); return 'ok'; });
await step('get alice', async () => { const s = await users.doc('alice').get(); return { exists: s.exists, data: s.data(), createEqualsUpdate: t(s.createTime) === t(s.updateTime), updateEqualsWrite: t(s.updateTime) === t(first.writeTime) }; });
await step('update dotted', async () => { await users.doc('alice').update({ age: 31, 'nested.y': 2 }); return (await users.doc('alice').get()).data(); });
await step('set merge', async () => { await users.doc('alice').set({ city: 'Tokyo', nested: { z: 3 } }, { merge: true }); return (await users.doc('alice').get()).data(); });
await step('create existing', async () => { await users.doc('alice').create({ x: 1 }); return 'unexpected'; });
await step('update missing', async () => { await users.doc('nobody').update({ x: 1 }); return 'unexpected'; });
await step('delete with stale lastUpdateTime', async () => { await users.doc('alice').delete({ lastUpdateTime: first.writeTime }); return 'unexpected'; });
await step('identical set keeps updateTime', async () => {
  const before = await users.doc('alice').get();
  const w = await users.doc('alice').set(before.data());
  const after = await users.doc('alice').get();
  return { writeTimeIsOldUpdateTime: t(w.writeTime) === t(before.updateTime), updateTimeUnchanged: t(after.updateTime) === t(before.updateTime) };
});
await step('getAll', async () => (await db.getAll(users.doc('nobody'), users.doc('alice'))).map(s => ({ id: s.id, exists: s.exists })));
await step('batch', async () => {
  const b = db.batch();
  b.set(users.doc('bob'), { n: 1 }); b.set(users.doc('carol'), { n: 2 }); b.delete(users.doc('alice'));
  b.set(db.doc('users/bob/posts/p1'), { title: 'hi' });
  b.set(db.doc('ghosts/g/items/i'), { n: 3 });
  const results = await b.commit();
  return results.length;
});
await step('listDocuments users', async () => (await users.listDocuments()).map(d => d.id));
await step('listDocuments ghosts (missing parent)', async () => (await db.collection('ghosts').listDocuments()).map(d => d.id));
await step('listCollections root', async () => (await db.listCollections()).map(c => c.id));
await step('listCollections bob', async () => (await users.doc('bob').listCollections()).map(c => c.id));
await step('bulkWriter', async () => {
  const bw = db.bulkWriter();
  const results = [];
  bw.onWriteError(() => false);
  const ops = [bw.create(users.doc('dave'), { n: 1 }), bw.create(users.doc('bob'), { n: 9 }), bw.update(users.doc('nobody'), { n: 1 }), bw.delete(users.doc('carol'))];
  for (const p of ops) results.push(p.then(() => 'ok', e => `error ${e.code}`));
  await bw.close();
  return Promise.all(results);
});
await step('after bulkWriter', async () => (await users.listDocuments()).map(d => d.id).sort());
await step('serverTimestamp (transforms, #23)', async () => { await users.doc('ts').set({ at: FieldValue.serverTimestamp() }); return 'ok'; });

console.log(JSON.stringify(out, null, 1));
await db.terminate();
