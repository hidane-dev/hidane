// Runs a rules-unit-testing suite under Security Rules — `assertSucceeds` / `assertFails` for a
// signed-in user, a signed-out user and `withSecurityRulesDisabled`: gets, lists, creates,
// updates, deletes, a listener, batches and transactions — and prints which assertions passed,
// with each error's code and message, so the transcripts of two emulators can be diffed.
//
// In Node (the web SDK over gRPC), run it from a directory outside the repository where
// `npm i firebase @firebase/rules-unit-testing` was run, against a freshly started emulator:
//   FIRESTORE_EMULATOR_HOST=127.0.0.1:8141 node sdk_rules_unit_testing.mjs > official.json
// In the browser (WebChannel), webchannel/rules_page.js runs the same steps.
import { assertFails, assertSucceeds, initializeTestEnvironment } from '@firebase/rules-unit-testing';
import rulesUnitTestingPackage from '@firebase/rules-unit-testing/package.json' with { type: 'json' };
import { SDK_VERSION } from 'firebase/app';
import {
  collection, deleteDoc, doc, getDoc, getDocs, onSnapshot, query, runTransaction, setDoc,
  updateDoc, where, writeBatch,
} from 'firebase/firestore';

export const projectId = 'demo-rules-unit-testing';

// Each kind of condition: `request.auth` and its claims, `resource.data`, `request.resource.data`,
// a `get()` lookup, and a list rule that only a filter on `public` satisfies.
export const rules = `rules_version = '2';
service cloud.firestore {
  match /databases/{database}/documents {
    function signedIn() {
      return request.auth != null;
    }
    function isAdmin() {
      return get(/databases/$(database)/documents/admins/$(request.auth.uid)).data.role == 'admin';
    }
    match /posts/{post} {
      allow get: if resource.data.public == true
        || (signedIn() && resource.data.owner == request.auth.uid);
      allow list: if resource.data.public == true;
      allow create: if signedIn()
        && request.resource.data.owner == request.auth.uid
        && request.auth.token.email_verified == true;
      allow update: if signedIn()
        && resource.data.owner == request.auth.uid
        && request.resource.data.owner == resource.data.owner;
      allow delete: if signedIn() && (resource.data.owner == request.auth.uid || isAdmin());
    }
    match /admins/{uid} {
      allow read: if signedIn() && request.auth.uid == uid;
    }
    match /counters/{counter} {
      allow read: if signedIn();
      allow update: if signedIn() && request.resource.data.n == resource.data.n + 1;
    }
  }
}
`;

// Documents come back in no fixed field order: print their keys sorted.
const sorted = (v) => (v && typeof v === 'object' && !Array.isArray(v)
  ? Object.fromEntries(Object.keys(v).sort().map((k) => [k, sorted(v[k])]))
  : Array.isArray(v) ? v.map(sorted) : v);
const read = (ref) => getDoc(ref).then((s) => (s.exists() ? sorted(s.data()) : null));
const ids = (q) => getDocs(q).then((s) => s.docs.map((d) => d.id));
// Resolves with the first snapshot from the server, rejects with the listener's error.
const listen = (q) => new Promise((resolve, reject) => {
  const unsubscribe = onSnapshot(q, { includeMetadataChanges: true }, (s) => {
    if (!s.metadata.fromCache) { unsubscribe(); resolve(s.docs.map((d) => d.id)); }
  }, reject);
});

export async function rulesUnitTesting({ host, port }) {
  const steps = [{ versions: { firebase: SDK_VERSION, '@firebase/rules-unit-testing': rulesUnitTestingPackage.version } }];
  async function check(step, expect, fn) {
    const entry = { step, expect };
    try {
      if (expect === 'succeeds') {
        const result = await assertSucceeds(fn());
        Object.assign(entry, { passed: true }, result === undefined ? {} : { result });
      } else {
        const e = await assertFails(fn());
        Object.assign(entry, { passed: true, code: e.code, message: e.message });
      }
    } catch (e) {
      Object.assign(entry, { passed: false, code: e.code, message: e.message ?? String(e) });
    }
    steps.push(entry);
  }
  const disabled = (fn) => async () => {
    let result;
    await testEnv.withSecurityRulesDisabled(async (ctx) => { result = await fn(ctx.firestore()); });
    return result;
  };

  let testEnv;
  await check('initializeTestEnvironment with rules', 'succeeds', async () => {
    testEnv = await initializeTestEnvironment({ projectId, firestore: { host, port, rules } });
  });
  const alice = testEnv.authenticatedContext('alice', { email: 'alice@example.com', email_verified: true }).firestore();
  const bob = testEnv.authenticatedContext('bob').firestore();
  const carol = testEnv.authenticatedContext('carol', { email_verified: false }).firestore();
  const signedOut = testEnv.unauthenticatedContext().firestore();

  await check('rules disabled: seed', 'succeeds', disabled(async (db) => {
    await setDoc(doc(db, 'admins/bob'), { role: 'admin' });
    await setDoc(doc(db, 'posts/p1'), { owner: 'alice', public: true, title: 'Hello' });
    await setDoc(doc(db, 'posts/p2'), { owner: 'alice', public: false, title: 'Draft' });
    await setDoc(doc(db, 'posts/p3'), { owner: 'bob', public: false, title: 'Secret' });
    await setDoc(doc(db, 'counters/c'), { n: 0 });
  }));
  await check('rules disabled: list posts', 'succeeds', disabled((db) => ids(collection(db, 'posts'))));

  // get
  await check('alice gets posts/p1 (public)', 'succeeds', () => read(doc(alice, 'posts/p1')));
  await check('alice gets posts/p2 (hers)', 'succeeds', () => read(doc(alice, 'posts/p2')));
  await check("alice gets posts/p3 (bob's)", 'fails', () => read(doc(alice, 'posts/p3')));
  await check('alice gets posts/missing', 'fails', () => read(doc(alice, 'posts/missing')));
  await check('alice gets admins/alice (missing, hers)', 'succeeds', () => read(doc(alice, 'admins/alice')));
  await check('alice gets admins/bob', 'fails', () => read(doc(alice, 'admins/bob')));
  await check('signed out gets posts/p1 (public)', 'succeeds', () => read(doc(signedOut, 'posts/p1')));
  await check('signed out gets posts/p2', 'fails', () => read(doc(signedOut, 'posts/p2')));

  // list
  await check('alice queries posts where public == true', 'succeeds', () => ids(query(collection(alice, 'posts'), where('public', '==', true))));
  await check('alice queries all posts', 'fails', () => ids(collection(alice, 'posts')));
  await check("alice queries posts where owner == 'alice'", 'fails', () => ids(query(collection(alice, 'posts'), where('owner', '==', 'alice'))));
  await check('signed out queries posts where public == true', 'succeeds', () => ids(query(collection(signedOut, 'posts'), where('public', '==', true))));
  await check('alice listens to posts where public == true', 'succeeds', () => listen(query(collection(alice, 'posts'), where('public', '==', true))));
  await check('alice listens to all posts', 'fails', () => listen(collection(alice, 'posts')));
  await check('signed out listens to posts/p2', 'fails', () => listen(doc(signedOut, 'posts/p2')));

  // create
  await check('alice creates posts/a1 (hers)', 'succeeds', () => setDoc(doc(alice, 'posts/a1'), { owner: 'alice', public: false, title: 'New' }));
  await check("alice creates posts/a2 (owner 'bob')", 'fails', () => setDoc(doc(alice, 'posts/a2'), { owner: 'bob', public: false }));
  await check('carol creates posts/c1 (email not verified)', 'fails', () => setDoc(doc(carol, 'posts/c1'), { owner: 'carol', public: false }));
  await check('bob creates posts/b0 (no email_verified claim)', 'fails', () => setDoc(doc(bob, 'posts/b0'), { owner: 'bob', public: false }));
  await check('signed out creates posts/s1', 'fails', () => setDoc(doc(signedOut, 'posts/s1'), { owner: null, public: true }));

  // update
  await check('alice updates posts/p2 (title)', 'succeeds', () => updateDoc(doc(alice, 'posts/p2'), { title: 'Edited' }));
  await check("alice updates posts/p2 (owner to 'bob')", 'fails', () => updateDoc(doc(alice, 'posts/p2'), { owner: 'bob' }));
  await check("alice updates posts/p3 (bob's)", 'fails', () => updateDoc(doc(alice, 'posts/p3'), { title: 'Mine' }));
  await check('signed out updates posts/p1', 'fails', () => updateDoc(doc(signedOut, 'posts/p1'), { title: 'Anonymous' }));

  // delete
  await check('alice deletes posts/a1 (hers)', 'succeeds', () => deleteDoc(doc(alice, 'posts/a1')));
  await check("alice deletes posts/p3 (bob's, not an admin)", 'fails', () => deleteDoc(doc(alice, 'posts/p3')));
  await check("bob deletes posts/p2 (alice's, admin by get())", 'succeeds', () => deleteDoc(doc(bob, 'posts/p2')));
  await check('signed out deletes posts/p1', 'fails', () => deleteDoc(doc(signedOut, 'posts/p1')));

  // batch
  await check('alice commits a batch of two creates', 'succeeds', () => {
    const b = writeBatch(alice);
    b.set(doc(alice, 'posts/b1'), { owner: 'alice', public: true });
    b.set(doc(alice, 'posts/b2'), { owner: 'alice', public: false });
    return b.commit();
  });
  await check("alice commits a batch with a denied update (posts/p3, bob's)", 'fails', () => {
    const b = writeBatch(alice);
    b.set(doc(alice, 'posts/b3'), { owner: 'alice', public: true });
    b.update(doc(alice, 'posts/p3'), { title: 'Mine' });
    return b.commit();
  });
  await check('rules disabled: posts/b3 was not written', 'succeeds', disabled((db) => read(doc(db, 'posts/b3'))));
  await check('signed out commits a batch deleting posts/b1', 'fails', () => {
    const b = writeBatch(signedOut);
    b.delete(doc(signedOut, 'posts/b1'));
    return b.commit();
  });

  // transaction
  await check('alice increments counters/c in a transaction (and reads posts/p1)', 'succeeds', () => runTransaction(alice, async (t) => {
    const c = await t.get(doc(alice, 'counters/c'));
    await t.get(doc(alice, 'posts/p1'));
    t.update(doc(alice, 'counters/c'), { n: c.data().n + 1 });
    return c.data().n + 1;
  }));
  await check('alice adds 2 to counters/c in a transaction', 'fails', () => runTransaction(alice, async (t) => {
    const c = await t.get(doc(alice, 'counters/c'));
    t.update(doc(alice, 'counters/c'), { n: c.data().n + 2 });
  }));
  await check("alice reads posts/p3 (bob's) in a transaction", 'fails', () => runTransaction(alice, async (t) => {
    await t.get(doc(alice, 'posts/p3'));
  }));
  await check('signed out increments counters/c in a transaction', 'fails', () => runTransaction(signedOut, async (t) => {
    const c = await t.get(doc(signedOut, 'counters/c'));
    t.update(doc(signedOut, 'counters/c'), { n: c.data().n + 1 });
  }));

  await check('rules disabled: read everything', 'succeeds', disabled(async (db) => {
    const posts = await getDocs(collection(db, 'posts'));
    return {
      posts: posts.docs.map((d) => [d.id, sorted(d.data())]),
      counter: await read(doc(db, 'counters/c')),
    };
  }));
  await check('clearFirestore()', 'succeeds', () => testEnv.clearFirestore());
  await check('rules disabled: list posts after clearFirestore()', 'succeeds', disabled((db) => ids(collection(db, 'posts'))));

  await testEnv.cleanup();
  return steps;
}

if (globalThis.process?.argv?.[1]?.endsWith('sdk_rules_unit_testing.mjs')) {
  const [host, port] = process.env.FIRESTORE_EMULATOR_HOST.split(':');
  rulesUnitTesting({ host, port: Number(port) }).then((steps) => {
    console.log(JSON.stringify(steps, null, 1));
    process.exit(0);
  });
}
