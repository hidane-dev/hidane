// Firestore-triggered functions, v1 and v2, that write down what they receive (#70): each event
// becomes a document of `received` holding the trigger's name, the event's attributes that do
// not change from run to run (no IDs or times) and the documents before and after, which
// `../drive.mjs` reads back. See its header for how to run them.
const v1 = require("firebase-functions/v1");
const firestore = require("firebase-functions/v2/firestore");
const { initializeApp } = require("firebase-admin/app");
const { GeoPoint, Timestamp, getFirestore } = require("firebase-admin/firestore");

const db = getFirestore(initializeApp());

function describe(v) {
  if (v === null || typeof v !== "object") return Number.isNaN(v) ? "NaN" : v;
  if (v instanceof Timestamp) return { timestamp: [v.seconds, v.nanoseconds] };
  if (v instanceof GeoPoint) return { geo: [v.latitude, v.longitude] };
  if (Buffer.isBuffer(v)) return { bytes: [...v] };
  if (typeof v.toArray === "function") return { vector: v.toArray() };
  if (typeof v.path === "string" && v.firestore) return { reference: v.path };
  if (Array.isArray(v)) return v.map(describe);
  return Object.fromEntries(Object.keys(v).sort().map((k) => [k, describe(v[k])]));
}

function snapshot(snap) {
  if (!snap) return null;
  return { path: snap.ref.path, exists: snap.exists, data: snap.exists ? describe(snap.data()) : null };
}

function record(trigger, info) {
  return db.collection("received").add({ trigger, info: JSON.stringify(info) });
}

function v1Context(context) {
  return { params: context.params, eventType: context.eventType, resource: context.resource };
}

const items = v1.firestore.document("items/{id}");
exports.v1create = items.onCreate((snap, context) =>
  record("v1create", { ...v1Context(context), after: snapshot(snap) }));
exports.v1update = items.onUpdate((change, context) =>
  record("v1update", { ...v1Context(context), before: snapshot(change.before), after: snapshot(change.after) }));
exports.v1delete = items.onDelete((snap, context) =>
  record("v1delete", { ...v1Context(context), before: snapshot(snap) }));
exports.v1write = items.onWrite((change, context) =>
  record("v1write", { ...v1Context(context), before: snapshot(change.before), after: snapshot(change.after) }));

function v2Event(event) {
  const out = {
    params: event.params, type: event.type, source: event.source, subject: event.subject,
    project: event.project, database: event.database, namespace: event.namespace,
    document: event.document, location: event.location,
  };
  if ("authType" in event) {
    out.authType = event.authType;
    out.authId = event.authId;
  }
  return out;
}

function v2Change(event) {
  const data = event.data;
  if (!data) return { data: null };
  if ("before" in data && "after" in data) return { before: snapshot(data.before), after: snapshot(data.after) };
  return { snapshot: snapshot(data) };
}

for (const [name, on] of [
  ["v2created", firestore.onDocumentCreated],
  ["v2updated", firestore.onDocumentUpdated],
  ["v2deleted", firestore.onDocumentDeleted],
  ["v2written", firestore.onDocumentWritten],
  ["v2writtenAuth", firestore.onDocumentWrittenWithAuthContext],
]) {
  exports[name] = on("items/{id}", (event) => record(name, { ...v2Event(event), ...v2Change(event) }));
}
exports.v2nested = firestore.onDocumentWritten("items/{id}/sub/{sub}", (event) =>
  record("v2nested", { ...v2Event(event), ...v2Change(event) }));
exports.v2named = firestore.onDocumentWritten({ document: "items/{id}", database: "named-db" }, (event) =>
  record("v2named", { ...v2Event(event), ...v2Change(event) }));
