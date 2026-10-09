// users コレクションに onSnapshot リスナーを張り続ける(Emulator UI のリアルタイム watcher の代替)。
// 環境変数: FIRESTORE_EMULATOR_HOST, LISTEN_LIMIT (任意: limit 付きクエリにする)
const { Firestore } = require('@google-cloud/firestore');
const db = new Firestore({ projectId: 'fake-project-id' });
let q = db.collection('users');
if (process.env.LISTEN_LIMIT) q = q.limit(parseInt(process.env.LISTEN_LIMIT, 10));
let n = 0, t0 = Date.now();
q.onSnapshot(snap => {
  n++;
  if (n % 20 === 0) console.log(`[listener] snapshots=${n} size=${snap.size} changes=${snap.docChanges().length} t=${((Date.now()-t0)/1000).toFixed(1)}s`);
}, err => { console.error('[listener] error', err.message); });
console.log('[listener] attached', process.env.LISTEN_LIMIT ? `limit=${process.env.LISTEN_LIMIT}` : 'unlimited');
