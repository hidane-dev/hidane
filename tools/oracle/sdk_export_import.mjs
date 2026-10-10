// An export / import round trip as firebase-tools runs it: `write` stores documents of every
// kind of value through the Admin SDK, `read` reads them back and prints them, keys sorted
// (#108). Run `write` under `--export-on-exit` with one emulator and `read` under `--import`
// with the other, then diff the transcripts:
//
//   firebase emulators:exec --only firestore --project demo-export-import --export-on-exit ./exported 'node sdk_export_import.mjs write'
//   firebase emulators:exec --only firestore --project demo-export-import --import ./exported 'node sdk_export_import.mjs read' > read.json
import { FieldValue, Firestore, GeoPoint, Timestamp } from '@google-cloud/firestore';

const db = new Firestore({ projectId: 'demo-export-import' });
const paths = ['users/alice', 'users/alice/posts/p1', 'users/bob', 'users/__id42__', 'orphan/x/children/y'];

if (process.argv[2] === 'write') {
  await db.doc('users/alice').set({
    name: 'Alice', age: 30, ratio: -0.25, huge: Number.MAX_SAFE_INTEGER, nan: NaN,
    joined: Timestamp.fromMillis(1700000000123), old: new Timestamp(-100, 500000000),
    tags: ['a', 1, null, { k: 'v' }], home: new GeoPoint(35.6, 139.7), raw: Buffer.from([0, 1, 255]),
    nested: { deep: { x: true }, emptyArray: [], emptyMap: {} }, empty: [], none: null,
    embedding: FieldValue.vector([0.5, 1.5]),
  });
  await db.doc('users/alice/posts/p1').set({ title: 'hello', author: db.doc('users/alice'), others: [db.doc('users/bob')] });
  await db.doc('users/bob').set({ name: 'Bob' });
  await db.doc('users/__id42__').set({ numeric: true });
  await db.doc('orphan/x/children/y').set({ parentMissing: true });
  console.log('written');
} else {
  const describe = (v) => {
    if (v === null || typeof v !== 'object') return Number.isNaN(v) ? 'NaN' : v;
    if (v instanceof Timestamp) return { timestamp: [v.seconds, v.nanoseconds] };
    if (v instanceof GeoPoint) return { geo: [v.latitude, v.longitude] };
    if (Buffer.isBuffer(v)) return { bytes: [...v] };
    if (typeof v.toArray === 'function') return { vector: v.toArray() };
    if (typeof v.path === 'string' && v.firestore) return { reference: v.path };
    if (Array.isArray(v)) return v.map(describe);
    return Object.fromEntries(Object.keys(v).sort().map((k) => [k, describe(v[k])]));
  };
  const out = [];
  for (const path of paths) {
    const snap = await db.doc(path).get();
    out.push({ path, exists: snap.exists, data: snap.exists ? describe(snap.data()) : null });
  }
  out.push({ collections: (await db.listCollections()).map((c) => c.id).sort() });
  console.log(JSON.stringify(out, null, 1));
}
await db.terminate();
