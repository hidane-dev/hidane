// Vector search as apps use it: the Admin SDK writes vectors (`FieldValue.vector`) and runs
// `findNearest` with each distance measure, a threshold, a distance field, a filter and a
// count; the web SDK writes and reads vectors (`vector()`). Prints what each step got, so the
// transcripts of two emulators can be diffed. FIRESTORE_EMULATOR_HOST selects the emulator;
// start it fresh (or POST /reset) first.
import { FieldValue, Firestore } from '@google-cloud/firestore';
import { initializeApp } from 'firebase/app';
import { connectFirestoreEmulator, doc, getDoc, getFirestore, setDoc, terminate, vector } from 'firebase/firestore';

const [host, port] = process.env.FIRESTORE_EMULATOR_HOST.split(':');
const projectId = 'demo-sdk-find-nearest';
const admin = new Firestore({ projectId });
const web = getFirestore(initializeApp({ projectId, apiKey: 'demo' }));
connectFirestoreEmulator(web, host, Number(port));

const steps = [];
async function step(name, fn) {
  try {
    steps.push({ step: name, result: await fn() });
  } catch (e) {
    steps.push({ step: name, error: `${e.code} ${e.details ?? e.message}` });
  }
}
const items = admin.collection('items');
const nearest = (options, query = items) => query.findNearest({ vectorField: 'embedding', queryVector: [1, 1], limit: 10, ...options });
const rows = (snapshot) => snapshot.docs.map((d) => [d.id, d.get('distance') ?? null]);

await step('Admin SDK writes vectors', async () => {
  const batch = admin.batch();
  const data = { a: [1, 0], b: [0, 1], c: [2, 2], d: [-1, -1], e: [0, 0] };
  for (const [id, v] of Object.entries(data)) batch.set(items.doc(id), { embedding: FieldValue.vector(v), tag: id < 'c' ? 'low' : 'high' });
  await batch.commit();
  return 'ok';
});
for (const measure of ['EUCLIDEAN', 'COSINE', 'DOT_PRODUCT']) {
  await step(`findNearest ${measure}`, async () => rows(await nearest({ distanceMeasure: measure, distanceResultField: 'distance' }).get()));
}
await step('findNearest with a threshold', async () => rows(await nearest({ distanceMeasure: 'EUCLIDEAN', distanceThreshold: 1.5 }).get()));
await step('findNearest after a filter', async () => rows(await nearest({ distanceMeasure: 'COSINE' }, items.where('tag', '==', 'high')).get()));
await step('findNearest, limit 2', async () => rows(await nearest({ distanceMeasure: 'EUCLIDEAN', limit: 2 }).get()));
await step('findNearest, limit 0', async () => rows(await nearest({ distanceMeasure: 'EUCLIDEAN', limit: 0 }).get()));
await step('findNearest with a query limit', async () => rows(await nearest({ distanceMeasure: 'EUCLIDEAN' }, items.limit(1)).get()));
await step('Admin SDK writes an empty vector', async () => { await items.doc('empty').set({ embedding: FieldValue.vector([]) }); return 'ok'; });
await step('Admin SDK reads a vector', async () => (await items.doc('c').get()).get('embedding').toArray());
await step('web SDK writes a vector', async () => { await setDoc(doc(web, 'items/w'), { embedding: vector([3, 4]) }); return 'ok'; });
await step('web SDK reads it back', async () => (await getDoc(doc(web, 'items/w'))).get('embedding').toArray());
await step('findNearest sees the web write', async () => rows(await nearest({ distanceMeasure: 'DOT_PRODUCT', limit: 1 }).get()));

console.log(JSON.stringify(steps, null, 1));
await terminate(web);
await admin.terminate();
process.exit(0);
