import { test, afterEach, after } from "node:test";
import assert from "node:assert/strict";
import { initializeApp } from "firebase-admin/app";
import { getFirestore } from "firebase-admin/firestore";

const project = "demo-hidane";
initializeApp({ projectId: project });
const db = getFirestore();

// Each test starts from an empty database, as with clearFirestore().
afterEach(() =>
  fetch(`http://${process.env.FIRESTORE_EMULATOR_HOST}/emulator/v1/projects/${project}/databases/(default)/documents`, { method: "DELETE" }));
after(() => db.terminate());

for (let i = 0; i < 200; i++) {
  test(`order ${i}`, async () => {
    const batch = db.batch();
    for (const [id, status] of [["a", "open"], ["b", "paid"], ["c", "paid"]]) {
      batch.set(db.collection("orders").doc(`${i}-${id}`), { customer: i, status });
    }
    await batch.commit();
    assert.equal((await db.collection("orders").doc(`${i}-a`).get()).get("status"), "open");
    assert.ok((await db.collection("orders").doc(`${i}-b`).get()).exists);
    const paid = await db.collection("orders").where("status", "==", "paid").get();
    assert.equal(paid.size, 2);
  });
}
