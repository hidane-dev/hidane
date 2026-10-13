// firestore.rules, tested with @firebase/rules-unit-testing against hidane. `npm test` runs this
// file under `firebase emulators:exec`, which `hidane exec` makes start hidane in place of the
// official emulator; rules-unit-testing finds it through the variables firebase-tools sets.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { after, before, beforeEach, test } from 'node:test';
import { assertFails, assertSucceeds, initializeTestEnvironment } from '@firebase/rules-unit-testing';
import { collection, doc, getDoc, getDocs, query, setDoc, where } from 'firebase/firestore';

let env;

before(async () => {
  env = await initializeTestEnvironment({
    // The --project in package.json. A `demo-` project needs no Firebase project or credentials.
    projectId: 'demo-hidane',
    firestore: { rules: readFileSync('firestore.rules', 'utf8') },
  });
});

beforeEach(async () => {
  await env.clearFirestore();
  // Documents the tests read, written as an administrator would: without rules.
  await env.withSecurityRulesDisabled(async (context) => {
    const db = context.firestore();
    await setDoc(doc(db, 'users/alice'), { name: 'Alice' });
    await setDoc(doc(db, 'posts/hello'), { author: 'alice', published: true });
    await setDoc(doc(db, 'posts/draft'), { author: 'alice', published: false });
  });
});

after(() => env.cleanup());

test('a user reads and writes their own profile', async () => {
  const db = env.authenticatedContext('alice').firestore();
  await assertSucceeds(getDoc(doc(db, 'users/alice')));
  await assertSucceeds(setDoc(doc(db, 'users/alice'), { name: 'Alice A.' }));
});

test("nobody reads or writes someone else's profile", async () => {
  const bob = env.authenticatedContext('bob').firestore();
  await assertFails(getDoc(doc(bob, 'users/alice')));
  await assertFails(setDoc(doc(bob, 'users/alice'), { name: 'Mallory' }));
  const signedOut = env.unauthenticatedContext().firestore();
  await assertFails(getDoc(doc(signedOut, 'users/alice')));
});

test('a draft is visible to its author only', async () => {
  await assertSucceeds(getDoc(doc(env.authenticatedContext('alice').firestore(), 'posts/draft')));
  await assertFails(getDoc(doc(env.authenticatedContext('bob').firestore(), 'posts/draft')));
  await assertSucceeds(getDoc(doc(env.unauthenticatedContext().firestore(), 'posts/hello')));
});

test('a list must ask for published posts only', async () => {
  const posts = collection(env.unauthenticatedContext().firestore(), 'posts');
  const published = await assertSucceeds(getDocs(query(posts, where('published', '==', true))));
  assert.deepEqual(published.docs.map((d) => d.id), ['hello']);
  await assertFails(getDocs(posts));
});

test("posts are created in their author's name", async () => {
  const db = env.authenticatedContext('bob').firestore();
  await assertSucceeds(setDoc(doc(db, 'posts/mine'), { author: 'bob', published: false }));
  await assertFails(setDoc(doc(db, 'posts/forged'), { author: 'alice', published: true }));
});
