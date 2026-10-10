// Reads and writes as the callers of test suites do — a signed-in user (rules-unit-testing's
// unsigned mock token), a signed-out user, rules disabled (`Bearer owner` from the web SDK) and
// the Admin SDK — and prints what each one got, so the transcripts of two emulators can be
// diffed. FIRESTORE_EMULATOR_HOST selects the emulator; start it fresh (or POST /reset) first.
import { Firestore } from '@google-cloud/firestore';
import { initializeTestEnvironment } from '@firebase/rules-unit-testing';
import { collection, doc, getDoc, getDocs, onSnapshot, setDoc } from 'firebase/firestore';

const [host, port] = process.env.FIRESTORE_EMULATOR_HOST.split(':');
const projectId = 'demo-sdk-auth';
const admin = new Firestore({ projectId });
const testEnv = await initializeTestEnvironment({ projectId, firestore: { host, port: Number(port) } });

const steps = [];
async function step(name, fn) {
  try {
    steps.push({ step: name, result: await fn() });
  } catch (e) {
    steps.push({ step: name, error: e.code ?? String(e) });
  }
}
const firstServerSnapshot = (query) => new Promise((resolve, reject) => {
  // A server snapshot equal to the cached one changes only metadata.
  const unsubscribe = onSnapshot(query, { includeMetadataChanges: true }, (s) => {
    if (!s.metadata.fromCache) { unsubscribe(); resolve(s.docs.map((d) => d.id)); }
  }, reject);
});

const alice = testEnv.authenticatedContext('alice', { email: 'alice@example.com', email_verified: true }).firestore();
const signedOut = testEnv.unauthenticatedContext().firestore();
await step('user writes c/a', () => setDoc(doc(alice, 'c/a'), { by: 'alice' }).then(() => 'ok'));
await step('user reads c/a', async () => (await getDoc(doc(alice, 'c/a'))).data());
await step('user listens to c', () => firstServerSnapshot(collection(alice, 'c')));
await step('signed-out user reads c/a', async () => (await getDoc(doc(signedOut, 'c/a'))).data());
await step('signed-out user queries c', async () => (await getDocs(collection(signedOut, 'c'))).docs.map((d) => d.id));
await step('rules disabled writes c/b', () =>
  testEnv.withSecurityRulesDisabled((ctx) => setDoc(doc(ctx.firestore(), 'c/b'), { by: 'owner' })).then(() => 'ok'));
await step('rules disabled lists c', async () => {
  let ids;
  await testEnv.withSecurityRulesDisabled(async (ctx) => { ids = (await getDocs(collection(ctx.firestore(), 'c'))).docs.map((d) => d.id); });
  return ids;
});
await step('Admin SDK lists collections', async () => (await admin.listCollections()).map((c) => c.id));
await step('Admin SDK reads c', async () => (await admin.collection('c').get()).docs.map((d) => [d.id, d.data()]));

console.log(JSON.stringify(steps, null, 1));
await testEnv.cleanup();
await admin.terminate();
process.exit(0);
