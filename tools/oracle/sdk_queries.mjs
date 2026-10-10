// Runs queries through @google-cloud/firestore (the engine of firebase-admin) and prints a
// transcript of document IDs, so the transcripts of two emulators can be diffed.
// FIRESTORE_EMULATOR_HOST selects the emulator; start it fresh (or POST /reset) first.
import { FieldPath, FieldValue, Filter, Firestore } from '@google-cloud/firestore';

const db = new Firestore({ projectId: 'demo-sdk-queries' });
const out = [];
async function step(name, fn) {
  try { out.push({ name, ok: await fn() }); }
  catch (e) { out.push({ name, error: { code: e.code, message: String(e.details ?? e.message).replace(/projects\/[^ "]*/g, '<name>') } }); }
}
const ids = (snap) => snap.docs.map((d) => d.ref.path);

const cities = db.collection('cities');
const data = {
  SF: { name: 'San Francisco', state: 'CA', country: 'USA', capital: false, population: 860000, regions: ['west_coast', 'norcal'], loc: { lat: 37.7 } },
  LA: { name: 'Los Angeles', state: 'CA', country: 'USA', capital: false, population: 3900000, regions: ['west_coast', 'socal'], loc: { lat: 34.0 } },
  DC: { name: 'Washington, D.C.', state: null, country: 'USA', capital: true, population: 680000, regions: ['east_coast'] },
  TOK: { name: 'Tokyo', state: null, country: 'Japan', capital: true, population: 9000000, regions: ['kanto', 'honshu'], loc: { lat: 35.7 } },
  BJ: { name: 'Beijing', state: null, country: 'China', capital: true, population: 21500000, regions: ['jingjinji', 'hebei'] },
  NAN: { name: 'Nowhere', country: 'None', population: NaN },
};
await step('seed', async () => {
  const batch = db.batch();
  for (const [id, d] of Object.entries(data)) batch.set(cities.doc(id), d);
  batch.set(db.doc('cities/SF/landmarks/golden-gate'), { name: 'Golden Gate Bridge', type: 'bridge' });
  batch.set(db.doc('cities/SF/landmarks/legion'), { name: 'Legion of Honor', type: 'museum' });
  batch.set(db.doc('cities/TOK/landmarks/national'), { name: 'National Museum', type: 'museum' });
  batch.set(db.doc('cities/BJ/landmarks/forbidden'), { name: 'Forbidden City', type: 'museum' });
  await batch.commit();
  return 'ok';
});

await step('all', async () => ids(await cities.get()));
await step('where ==', async () => ids(await cities.where('state', '==', 'CA').get()));
await step('where == null (IS_NULL)', async () => ids(await cities.where('state', '==', null).get()));
await step('where != null (IS_NOT_NULL)', async () => ids(await cities.where('state', '!=', null).get()));
await step('where == NaN (IS_NAN)', async () => ids(await cities.where('population', '==', NaN).get()));
await step('where <', async () => ids(await cities.where('population', '<', 1000000).get()));
await step('where >= orderBy desc', async () => ids(await cities.where('population', '>=', 1000000).orderBy('population', 'desc').get()));
await step('where !=', async () => ids(await cities.where('country', '!=', 'USA').get()));
await step('where in', async () => ids(await cities.where('country', 'in', ['Japan', 'China']).get()));
await step('where not-in', async () => ids(await cities.where('country', 'not-in', ['USA', 'Japan']).get()));
await step('array-contains', async () => ids(await cities.where('regions', 'array-contains', 'west_coast').get()));
await step('array-contains-any', async () => ids(await cities.where('regions', 'array-contains-any', ['west_coast', 'kanto']).get()));
await step('nested field', async () => ids(await cities.where('loc.lat', '>', 35).get()));
await step('two inequalities', async () => ids(await cities.where('population', '>', 700000).where('name', '<', 'U').get()));
await step('Filter.or', async () => ids(await cities.where(Filter.or(Filter.where('capital', '==', true), Filter.where('state', '==', 'CA'))).get()));
await step('Filter.or of and', async () => ids(await cities.where(Filter.or(Filter.and(Filter.where('state', '==', 'CA'), Filter.where('population', '>', 1000000)), Filter.where('country', '==', 'Japan'))).get()));
await step('orderBy limit', async () => ids(await cities.orderBy('name').limit(3).get()));
await step('limitToLast', async () => ids(await cities.orderBy('name').limitToLast(2).get()));
await step('offset', async () => ids(await cities.orderBy('population').offset(2).limit(2).get()));
await step('startAt / endBefore values', async () => ids(await cities.orderBy('population').startAt(860000).endBefore(9000000).get()));
await step('startAfter a snapshot', async () => {
  const sf = await cities.doc('SF').get();
  return ids(await cities.orderBy('country').startAfter(sf).get());
});
await step('paging with startAfter', async () => {
  const pages = [];
  let q = cities.orderBy('population').limit(2);
  for (;;) {
    const snap = await q.get();
    if (snap.empty) break;
    pages.push(ids(snap));
    q = cities.orderBy('population').startAfter(snap.docs[snap.docs.length - 1]).limit(2);
  }
  return pages;
});
await step('select', async () => (await cities.where('capital', '==', true).select('name', 'loc.lat').get()).docs.map((d) => ({ id: d.id, data: d.data() })));
await step('documentId in', async () => ids(await cities.where(FieldPath.documentId(), 'in', ['TOK', 'SF']).get()));
await step('documentId range', async () => ids(await cities.where(FieldPath.documentId(), '>=', 'L').where(FieldPath.documentId(), '<', 'T').get()));
await step('orderBy documentId desc', async () => ids(await cities.orderBy(FieldPath.documentId(), 'desc').get()));
await step('collectionGroup', async () => ids(await db.collectionGroup('landmarks').where('type', '==', 'museum').get()));
await step('collectionGroup orderBy name', async () => ids(await db.collectionGroup('landmarks').orderBy('name').get()));
await step('subcollection', async () => ids(await cities.doc('SF').collection('landmarks').get()));
await step('stream', async () => {
  const got = [];
  for await (const doc of cities.where('capital', '==', true).stream()) got.push(doc.id);
  return got;
});
await step('in with 31 values', async () => ids(await cities.where('population', 'in', Array.from({ length: 31 }, (_, i) => i)).get()));
await step('query in a transaction', async () => db.runTransaction(async (tx) => {
  const snap = await tx.get(cities.where('capital', '==', true));
  tx.update(cities.doc('DC'), { visited: true });
  return ids(snap);
}));
await step('read-only transaction query', async () => db.runTransaction(async (tx) => ids(await tx.get(cities.where('state', '==', 'CA'))), { readOnly: true }));
await step('recursiveDelete a document', async () => {
  await db.recursiveDelete(cities.doc('SF'));
  return { cities: ids(await cities.get()), landmarks: ids(await db.collectionGroup('landmarks').get()) };
});
await step('recursiveDelete a collection', async () => {
  await db.recursiveDelete(cities);
  return { cities: ids(await cities.get()), landmarks: ids(await db.collectionGroup('landmarks').get()) };
});

console.log(JSON.stringify(out, null, 1));
await db.terminate();
