// hidane's container image through Testcontainers for Node (docs/testcontainers.md): in place of
// the Cloud SDK image in @testcontainers/gcloud's FirestoreEmulatorContainer, which starts it with
// `/bin/sh -c "gcloud beta emulators firestore start --host-port=0.0.0.0:8080 <flags>"` and waits
// for a log line with "running", and as a GenericContainer. HIDANE_IMAGE names the image to test.
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { test } from "node:test";

import { FirestoreEmulatorContainer } from "@testcontainers/gcloud";
import { deleteApp, initializeApp } from "firebase-admin/app";
import { FieldValue, getFirestore } from "firebase-admin/firestore";
import { GenericContainer, Wait } from "testcontainers";

const IMAGE = process.env.HIDANE_IMAGE;
if (!IMAGE) {
  throw new Error("Set HIDANE_IMAGE to the hidane image to test, such as ghcr.io/hidane-dev/hidane:<version>");
}
// GENERIC_ONLY=1 runs the GenericContainer test only, for images older than the gcloud support.
const genericOnly = process.env.GENERIC_ONLY === "1";

/**
 * A firebase-admin Firestore client for the emulator at `host:port`. FIRESTORE_EMULATOR_HOST also
 * tells the SDK not to look for credentials (`settings({ host, ssl: false })` alone connects too,
 * after a metadata server lookup that times out). The tests run one at a time.
 */
function client(endpoint) {
  process.env.FIRESTORE_EMULATOR_HOST = endpoint;
  const app = initializeApp({ projectId: "demo-testcontainers" }, `app-${randomUUID()}`);
  return { app, db: getFirestore(app) };
}

/** Writes, reads, queries, a transaction, a batch and a listener. */
async function run(endpoint) {
  const { app, db } = client(endpoint);
  try {
    const users = db.collection("users");
    await users.doc("alovelace").set({ first: "Ada", last: "Lovelace", born: 1815 });
    await users.doc("aturing").set({ first: "Alan", last: "Turing", born: 1912 });
    await users.doc("ghopper").set({ first: "Grace", last: "Hopper", born: 1906 });

    const ada = await users.doc("alovelace").get();
    assert.deepEqual(ada.data(), { first: "Ada", last: "Lovelace", born: 1815 });

    const modern = await users.where("born", ">", 1900).orderBy("born").get();
    assert.deepEqual(
      modern.docs.map((d) => d.id),
      ["ghopper", "aturing"],
    );

    const counter = db.collection("counters").doc("visits");
    await counter.set({ count: 0 });
    for (let i = 0; i < 3; i++) {
      await db.runTransaction(async (tx) => {
        const count = (await tx.get(counter)).get("count");
        tx.update(counter, { count: count + 1 });
      });
    }
    await counter.update({ count: FieldValue.increment(10) });
    assert.equal((await counter.get()).get("count"), 13);

    const batch = db.batch();
    batch.delete(users.doc("aturing"));
    batch.update(users.doc("ghopper"), { rank: "Rear Admiral" });
    await batch.commit();
    assert.equal((await users.doc("aturing").get()).exists, false);

    const ids = await new Promise((resolve, reject) => {
      const unsubscribe = users.orderBy("born", "desc").onSnapshot(
        (snapshot) => {
          unsubscribe();
          resolve(snapshot.docs.map((d) => d.id));
        },
        reject,
      );
    });
    assert.deepEqual(ids, ["ghopper", "alovelace"]);
  } finally {
    await deleteApp(app);
  }
}

test("the gcloud module starts hidane", { skip: genericOnly, timeout: 240_000 }, async () => {
  const container = await new FirestoreEmulatorContainer(IMAGE).start();
  try {
    await run(container.getEmulatorEndpoint());
  } finally {
    await container.stop();
  }
});

test("the gcloud module passes its flags to hidane", { skip: genericOnly, timeout: 240_000 }, async () => {
  let logs = "";
  const container = await new FirestoreEmulatorContainer(IMAGE)
    .withFlag("project", "demo-testcontainers")
    .withLogConsumer((stream) => stream.on("data", (line) => (logs += line)))
    .start();
  try {
    // hidane says what it serves on stderr, which reaches the consumer apart from stdout.
    const note = "in place of `gcloud emulators firestore start`, with: hidane --host 0.0.0.0 --port 8080";
    for (let i = 0; i < 100 && !logs.includes(note); i++) {
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    assert.ok(logs.includes(note), logs);
    await run(container.getEmulatorEndpoint());
  } finally {
    await container.stop();
  }
});

test("a GenericContainer starts hidane", { timeout: 240_000 }, async () => {
  const container = await new GenericContainer(IMAGE)
    .withExposedPorts(8080)
    .withWaitStrategy(Wait.forLogMessage("Dev App Server is now running."))
    .start();
  try {
    await run(`${container.getHost()}:${container.getMappedPort(8080)}`);
  } finally {
    await container.stop();
  }
});
