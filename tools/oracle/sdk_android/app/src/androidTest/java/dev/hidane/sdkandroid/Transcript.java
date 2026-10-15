// Runs one section of steps through firebase-android-sdk's firebase-firestore in an Android
// emulator and writes a transcript without times or generated IDs, so the transcripts of two
// emulators can be diffed. The section and the emulator's port are instrumentation arguments
// (`-e section documents -e port 8080`); the emulator is reached on the host, 10.0.2.2. The
// transcript is written to the app's external files directory as `<section>.json`.
//
// The steps are those of tools/oracle/sdk_ios/ (the client SDKs offer the same ones): reads go to
// the server and the cache stays in memory, so a transcript holds what the emulator answered. The
// listen section writes through a second instance, as another client would, and leaves its
// listeners idle for 95 seconds once (firebase-tools#11238). Start the emulator fresh for each
// section.

package dev.hidane.sdkandroid;

import android.content.Context;
import android.os.Bundle;

import androidx.test.ext.junit.runners.AndroidJUnit4;
import androidx.test.platform.app.InstrumentationRegistry;

import com.google.android.gms.tasks.Task;
import com.google.android.gms.tasks.Tasks;
import com.google.firebase.FirebaseApp;
import com.google.firebase.FirebaseOptions;
import com.google.firebase.Timestamp;
import com.google.firebase.firestore.AggregateField;
import com.google.firebase.firestore.AggregateQuerySnapshot;
import com.google.firebase.firestore.AggregateSource;
import com.google.firebase.firestore.Blob;
import com.google.firebase.firestore.CollectionReference;
import com.google.firebase.firestore.DocumentChange;
import com.google.firebase.firestore.DocumentReference;
import com.google.firebase.firestore.DocumentSnapshot;
import com.google.firebase.firestore.FieldPath;
import com.google.firebase.firestore.FieldValue;
import com.google.firebase.firestore.Filter;
import com.google.firebase.firestore.FirebaseFirestore;
import com.google.firebase.firestore.FirebaseFirestoreException;
import com.google.firebase.firestore.FirebaseFirestoreSettings;
import com.google.firebase.firestore.GeoPoint;
import com.google.firebase.firestore.ListenerRegistration;
import com.google.firebase.firestore.MemoryCacheSettings;
import com.google.firebase.firestore.Query;
import com.google.firebase.firestore.QueryDocumentSnapshot;
import com.google.firebase.firestore.SetOptions;
import com.google.firebase.firestore.Source;
import com.google.firebase.firestore.VectorValue;
import com.google.firebase.firestore.WriteBatch;

import org.junit.Test;
import org.junit.runner.RunWith;

import java.io.File;
import java.io.FileOutputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Base64;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.Executor;
import java.util.concurrent.Executors;
import java.util.concurrent.atomic.AtomicInteger;

@RunWith(AndroidJUnit4.class)
public class Transcript {
    private String section;
    private FirebaseFirestore db;
    private FirebaseFirestore writer;
    private final List<Object> out = new ArrayList<>();

    @Test
    public void run() throws Exception {
        Bundle args = InstrumentationRegistry.getArguments();
        section = args.getString("section", "documents");
        int port = Integer.parseInt(args.getString("port", "8080"));
        Context context = InstrumentationRegistry.getInstrumentation().getTargetContext();
        db = instance(context, "client", port);
        writer = instance(context, "writer", port);
        switch (section) {
            case "documents": documents(); break;
            case "queries": queries(); break;
            case "transactions": transactions(); break;
            case "listen": listen(); break;
            default: throw new IllegalArgumentException("unknown section " + section);
        }
        File file = new File(context.getExternalFilesDir(null), section + ".json");
        try (FileOutputStream stream = new FileOutputStream(file)) {
            stream.write((json(out, "") + "\n").getBytes(StandardCharsets.UTF_8));
        }
    }

    /** A Firestore instance of its own app, talking to the emulator, its cache in memory. */
    private FirebaseFirestore instance(Context context, String name, int port) {
        FirebaseOptions options = new FirebaseOptions.Builder()
                .setProjectId("demo-android-" + section)
                .setApplicationId("1:1:android:1")
                .setApiKey("demo")
                .build();
        FirebaseApp app = FirebaseApp.initializeApp(context, options, name);
        FirebaseFirestore firestore = FirebaseFirestore.getInstance(app);
        firestore.useEmulator("10.0.2.2", port);
        firestore.setFirestoreSettings(new FirebaseFirestoreSettings.Builder()
                .setLocalCacheSettings(MemoryCacheSettings.newBuilder().build())
                .build());
        return firestore;
    }

    // --- values ---------------------------------------------------------------------------------

    private static Object number(double d) {
        if (Double.isNaN(d)) return map("double", "NaN");
        if (Double.isInfinite(d)) return map("double", d > 0 ? "Infinity" : "-Infinity");
        return map("double", Double.toString(d));
    }

    /** A Firestore value as JSON: doubles tagged, timestamps as "timestamp" (or their seconds
     * and nanoseconds when `times` is set), the other types tagged. */
    private static Object norm(Object v, boolean times) {
        if (v == null) return null;
        if (v instanceof Boolean || v instanceof String || v instanceof Long) return v;
        if (v instanceof Double) return number((Double) v);
        if (v instanceof Blob) return map("bytes", Base64.getEncoder().encodeToString(((Blob) v).toBytes()));
        if (v instanceof Timestamp) {
            Timestamp t = (Timestamp) v;
            return times ? map("timestamp", Arrays.asList(t.getSeconds(), (long) t.getNanoseconds())) : "timestamp";
        }
        if (v instanceof GeoPoint) {
            GeoPoint g = (GeoPoint) v;
            return map("geopoint", Arrays.asList(number(g.getLatitude()), number(g.getLongitude())));
        }
        if (v instanceof DocumentReference) return map("reference", ((DocumentReference) v).getPath());
        if (v instanceof VectorValue) {
            List<Object> values = new ArrayList<>();
            for (double d : ((VectorValue) v).toArray()) values.add(number(d));
            return map("vector", values);
        }
        if (v instanceof List) {
            List<Object> list = new ArrayList<>();
            for (Object e : (List<?>) v) list.add(norm(e, times));
            return list;
        }
        if (v instanceof Map) {
            Map<String, Object> m = new TreeMap<>();
            for (Map.Entry<?, ?> e : ((Map<?, ?>) v).entrySet()) m.put((String) e.getKey(), norm(e.getValue(), times));
            return m;
        }
        return map("unknown", v.getClass().getName());
    }

    private static Map<String, Object> map(Object... kv) {
        Map<String, Object> m = new TreeMap<>();
        for (int i = 0; i < kv.length; i += 2) m.put((String) kv[i], kv[i + 1]);
        return m;
    }

    /** An error as the code the SDK gives it; one of the harness's own as its message. The
     * messages are left out: the official emulator's print its internal keys. */
    private static Object errorOf(Throwable error) {
        Throwable cause = error;
        while (cause instanceof ExecutionException && cause.getCause() != null) cause = cause.getCause();
        if (cause instanceof FirebaseFirestoreException) {
            return map("code", ((FirebaseFirestoreException) cause).getCode().name());
        }
        return map("client", String.valueOf(cause.getMessage()));
    }

    // --- JSON -----------------------------------------------------------------------------------

    private static String json(Object v, String indent) {
        if (v == null) return "null";
        if (v instanceof Boolean || v instanceof Long || v instanceof Integer) return v.toString();
        if (v instanceof String) return quote((String) v);
        String inner = indent + "  ";
        if (v instanceof List) {
            List<?> list = (List<?>) v;
            if (list.isEmpty()) return "[]";
            StringBuilder s = new StringBuilder("[\n");
            for (int i = 0; i < list.size(); i++) {
                s.append(inner).append(json(list.get(i), inner)).append(i + 1 < list.size() ? ",\n" : "\n");
            }
            return s.append(indent).append("]").toString();
        }
        Map<?, ?> m = new TreeMap<>((Map<?, ?>) v);
        if (m.isEmpty()) return "{}";
        StringBuilder s = new StringBuilder("{\n");
        int i = 0;
        for (Map.Entry<?, ?> e : m.entrySet()) {
            s.append(inner).append(quote((String) e.getKey())).append(": ").append(json(e.getValue(), inner))
                    .append(++i < m.size() ? ",\n" : "\n");
        }
        return s.append(indent).append("}").toString();
    }

    private static String quote(String s) {
        StringBuilder out = new StringBuilder("\"");
        for (char c : s.toCharArray()) {
            switch (c) {
                case '"': out.append("\\\""); break;
                case '\\': out.append("\\\\"); break;
                case '\n': out.append("\\n"); break;
                default:
                    if (c < 0x20) out.append(String.format("\\u%04x", (int) c));
                    else out.append(c);
            }
        }
        return out.append('"').toString();
    }

    // --- the transcript -------------------------------------------------------------------------

    interface Body { Object run() throws Exception; }

    private void step(String name, Body body) {
        try {
            out.add(map("name", name, "ok", body.run()));
        } catch (Exception e) {
            out.add(map("name", name, "error", errorOf(e)));
        }
    }

    private static <T> T await(Task<T> task) throws Exception {
        return Tasks.await(task);
    }

    /** A document as the server has it: its data, or null when it does not exist. */
    private static Object read(DocumentReference ref, boolean times) throws Exception {
        DocumentSnapshot snap = await(ref.get(Source.SERVER));
        return snap.exists() ? norm(snap.getData(), times) : null;
    }

    private static Object read(DocumentReference ref) throws Exception {
        return read(ref, false);
    }

    private static Object ids(Query query) throws Exception {
        List<Object> paths = new ArrayList<>();
        for (QueryDocumentSnapshot doc : await(query.get(Source.SERVER))) paths.add(doc.getReference().getPath());
        return paths;
    }

    private static Map<String, Object> data(Object... kv) {
        Map<String, Object> m = new HashMap<>();
        for (int i = 0; i < kv.length; i += 2) m.put((String) kv[i], kv[i + 1]);
        return m;
    }

    // --- documents ------------------------------------------------------------------------------

    private void documents() {
        DocumentReference alice = db.document("users/alice");
        step("set alice", () -> {
            await(alice.set(data("name", "Alice", "age", 30L, "tags", Arrays.asList("a", "b"), "nested", data("x", 1L))));
            return read(alice);
        });
        step("update dotted", () -> {
            await(alice.update(data("age", 31L, "nested.y", 2L)));
            return read(alice);
        });
        step("update with a FieldPath holding a dot", () -> {
            await(alice.update(FieldPath.of("a.b"), 1L));
            return read(alice);
        });
        step("set merge", () -> {
            await(alice.set(data("city", "Tokyo", "nested", data("z", 3L)), SetOptions.merge()));
            return read(alice);
        });
        step("set mergeFields", () -> {
            await(alice.set(data("city", "Osaka", "age", 99L), SetOptions.mergeFields("city")));
            return read(alice);
        });
        step("delete a field", () -> {
            await(alice.update("city", FieldValue.delete()));
            return read(alice);
        });
        step("update a missing document", () -> {
            await(db.document("users/nobody").update("x", 1L));
            return "unexpected";
        });
        step("get a missing document", () -> read(db.document("users/nobody")));
        step("transforms", () -> {
            await(alice.update(data("age", FieldValue.increment(2), "score", FieldValue.increment(1.5),
                    "tags", FieldValue.arrayUnion("c", "a"), "nested.x", FieldValue.delete(),
                    "seen", FieldValue.serverTimestamp())));
            return read(alice);
        });
        step("arrayRemove", () -> {
            await(alice.update("tags", FieldValue.arrayRemove("a")));
            return read(alice);
        });
        step("server timestamp is a timestamp", () -> await(alice.get(Source.SERVER)).get("seen") instanceof Timestamp);
        step("a value of each type", () -> {
            DocumentReference types = db.document("users/types");
            Map<String, Object> values = data(
                    "int", 1L, "big", Long.MAX_VALUE, "double", 1.5, "whole double", 2.0, "negative zero", -0.0,
                    "nan", Double.NaN, "infinity", Double.POSITIVE_INFINITY, "minus infinity", Double.NEGATIVE_INFINITY,
                    "string", "x", "empty", "", "bool", true, "null", null,
                    "bytes", Blob.fromBytes(new byte[] {1, 2, (byte) 255}),
                    "timestamp", new Timestamp(1_577_836_800L, 123_456_000),
                    "geopoint", new GeoPoint(1.5, -2), "reference", db.document("users/alice"),
                    "array", Arrays.asList(1L, "a", data("m", true)), "map", data("a", data("b", 1L)),
                    "empty map", new HashMap<String, Object>(),
                    "vector", FieldValue.vector(new double[] {1, 2.5}));
            await(types.set(values));
            return read(types, true);
        });
        step("a batch", () -> {
            WriteBatch batch = db.batch();
            batch.set(db.document("b/one"), data("n", 1L));
            batch.set(db.document("b/two"), data("n", 2L));
            batch.update(db.document("b/one"), "n", 3L);
            batch.delete(db.document("b/two"));
            await(batch.commit());
            return Arrays.asList(read(db.document("b/one")), read(db.document("b/two")));
        });
        step("a batch that fails writes nothing", () -> {
            WriteBatch batch = db.batch();
            batch.set(db.document("b/three"), data("n", 1L));
            batch.update(db.document("b/missing"), "n", 1L);
            try {
                await(batch.commit());
                return "unexpected";
            } catch (Exception e) {
                return map("error", errorOf(e), "three", read(db.document("b/three")));
            }
        });
        step("a generated ID", () -> {
            DocumentReference ref = db.collection("generated").document();
            await(ref.set(data("n", 1L)));
            return map("length", (long) ref.getId().length(), "data", read(ref));
        });
        step("delete alice", () -> {
            await(alice.delete());
            return read(alice);
        });
        step("delete a missing document", () -> {
            await(db.document("users/nobody").delete());
            return "ok";
        });
    }

    // --- queries --------------------------------------------------------------------------------

    private void queries() {
        CollectionReference cities = db.collection("cities");
        step("seed", () -> {
            WriteBatch batch = db.batch();
            batch.set(cities.document("SF"), data("pop", 860L, "state", "CA", "tags", Arrays.asList("coast", "tech"), "area", 121.4));
            batch.set(cities.document("LA"), data("pop", 3900L, "state", "CA", "tags", Arrays.asList("coast"), "area", 1302.0));
            batch.set(cities.document("TOK"), data("pop", 9000L, "state", null, "tags", Arrays.asList("tech"), "area", 2194.0));
            batch.set(cities.document("NY"), data("pop", 8300L, "state", "NY", "tags", new ArrayList<>(), "area", 783.8));
            batch.set(cities.document("BJ"), data("pop", 21500L, "area", 16411.0));
            batch.set(db.document("cities/SF/landmarks/gg"), data("name", "Golden Gate"));
            batch.set(db.document("cities/LA/landmarks/hw"), data("name", "Hollywood"));
            batch.set(db.document("other/x/landmarks/e"), data("name", "Elsewhere"));
            await(batch.commit());
            return "ok";
        });
        step("all", () -> ids(cities));
        step("pop > 1000 by pop", () -> ids(cities.whereGreaterThan("pop", 1000).orderBy("pop")));
        step("state == CA", () -> ids(cities.whereEqualTo("state", "CA")));
        step("state == null", () -> ids(cities.whereEqualTo("state", null)));
        step("state != CA", () -> ids(cities.whereNotEqualTo("state", "CA")));
        step("state in [CA, NY]", () -> ids(cities.whereIn("state", Arrays.asList("CA", "NY"))));
        step("state not-in [CA]", () -> ids(cities.whereNotIn("state", Arrays.asList("CA"))));
        step("tags array-contains tech", () -> ids(cities.whereArrayContains("tags", "tech")));
        step("tags array-contains-any", () -> ids(cities.whereArrayContainsAny("tags", Arrays.asList("coast", "none"))));
        step("or", () -> ids(cities.where(Filter.or(Filter.equalTo("state", "NY"), Filter.lessThan("pop", 1000)))));
        step("and of an or", () -> ids(cities.where(Filter.and(Filter.greaterThan("area", 500),
                Filter.or(Filter.equalTo("state", "CA"), Filter.greaterThan("pop", 10000))))));
        step("top 2 by pop", () -> ids(cities.orderBy("pop", Query.Direction.DESCENDING).limit(2)));
        step("last 2 by pop", () -> ids(cities.orderBy("pop").limitToLast(2)));
        step("cursors", () -> ids(cities.orderBy("pop").startAt(1000).endBefore(9000)));
        step("cursor after a document", () -> {
            DocumentSnapshot la = await(cities.document("LA").get(Source.SERVER));
            return ids(cities.orderBy("pop").startAfter(la));
        });
        step("two inequalities", () -> ids(cities.whereGreaterThan("pop", 1000).whereLessThan("area", 2000)));
        step("document ID in", () -> ids(cities.whereIn(FieldPath.documentId(), Arrays.asList("SF", "NY", "XX"))));
        step("order by document ID, descending", () -> ids(cities.orderBy(FieldPath.documentId(), Query.Direction.DESCENDING)));
        step("collection group", () -> ids(db.collectionGroup("landmarks")));
        step("collection group, filtered", () -> ids(db.collectionGroup("landmarks").whereGreaterThan("name", "G")));
        step("count", () -> norm(await(cities.whereEqualTo("state", "CA").count().get(AggregateSource.SERVER)).getCount(), false));
        step("sum and average", () -> {
            AggregateQuerySnapshot snap = await(cities.aggregate(AggregateField.sum("pop"),
                    AggregateField.average("area"), AggregateField.count()).get(AggregateSource.SERVER));
            return Arrays.asList(norm(snap.get(AggregateField.sum("pop")), false),
                    norm(snap.get(AggregateField.average("area")), false), norm(snap.get(AggregateField.count()), false));
        });
        step("average of a missing field", () -> norm(await(cities.aggregate(AggregateField.average("nope"))
                .get(AggregateSource.SERVER)).get(AggregateField.average("nope")), false));
        step("a query an index would serve, ordered by a missing field", () -> ids(cities.orderBy("missing")));
    }

    // --- transactions ---------------------------------------------------------------------------

    private void transactions() {
        DocumentReference counter = db.document("t/counter");
        step("create in a transaction", () -> {
            Boolean existed = await(db.runTransaction(transaction -> {
                boolean exists = transaction.get(counter).exists();
                transaction.set(counter, data("n", 1L));
                return exists;
            }));
            return map("existed", existed, "now", read(counter));
        });
        step("read and increment", () -> {
            Long returned = await(db.runTransaction(transaction -> {
                Long n = transaction.get(counter).getLong("n");
                long next = (n == null ? 0 : n) + 1;
                transaction.update(counter, "n", next);
                return next;
            }));
            return map("returned", returned, "now", read(counter));
        });
        step("write only", () -> {
            await(db.runTransaction(transaction -> {
                transaction.set(db.document("t/write-only"), data("w", true));
                return null;
            }));
            return read(db.document("t/write-only"));
        });
        step("update a missing document", () -> {
            await(db.runTransaction(transaction -> {
                transaction.update(db.document("t/missing"), "n", 1L);
                return null;
            }));
            return "unexpected";
        });
        step("a callback that fails", () -> {
            try {
                await(db.runTransaction(transaction -> {
                    transaction.set(counter, data("n", 99L));
                    throw new IllegalStateException("the callback failed");
                }));
                return "unexpected";
            } catch (Exception e) {
                return map("error", errorOf(e), "now", read(counter));
            }
        });
        step("an outside write between the read and the commit", () -> {
            AtomicInteger attempts = new AtomicInteger();
            Long returned = await(db.runTransaction(transaction -> {
                Long n = transaction.get(counter).getLong("n");
                if (attempts.incrementAndGet() == 1) {
                    // Another client writes the document the transaction read.
                    try {
                        Tasks.await(writer.document("t/counter").set(data("n", 100L)));
                    } catch (Exception e) {
                        throw new IllegalStateException(e);
                    }
                }
                long next = (n == null ? 0 : n) + 1;
                transaction.update(counter, "n", next);
                return next;
            }));
            return map("returned", returned, "retried", attempts.get() > 1, "now", read(counter));
        });
        step("a batch after the transactions", () -> {
            WriteBatch batch = db.batch();
            batch.update(counter, "n", FieldValue.increment(10));
            await(batch.commit());
            return read(counter);
        });
    }

    // --- listen ---------------------------------------------------------------------------------

    private final Object lock = new Object();
    private final List<Object[]> entries = new ArrayList<>();
    private long last = System.currentTimeMillis();

    private void record(String listener, Map<String, Object> entry) {
        synchronized (lock) {
            entries.add(new Object[] {listener, entry});
            last = System.currentTimeMillis();
        }
    }

    /** Waits until no snapshot came for 300 ms (at least 150 ms), then takes what came. */
    private List<Object[]> quiet() throws InterruptedException {
        Thread.sleep(150);
        while (true) {
            synchronized (lock) {
                if (System.currentTimeMillis() - last >= 300) {
                    List<Object[]> taken = new ArrayList<>(entries);
                    entries.clear();
                    return taken;
                }
            }
            Thread.sleep(50);
        }
    }

    /** Waits until each of `names` has sent a snapshot (at most a minute): on a slow device the
     * first snapshots can take longer than the quiet time of a step. */
    private void awaitFirst(List<String> names) throws InterruptedException {
        long deadline = System.currentTimeMillis() + 60_000;
        while (System.currentTimeMillis() < deadline) {
            synchronized (lock) {
                List<String> seen = new ArrayList<>();
                for (Object[] e : entries) seen.add((String) e[0]);
                if (seen.containsAll(names)) return;
            }
            Thread.sleep(50);
        }
    }

    interface Action { void run() throws Exception; }

    private void listenStep(String name, Action action) throws InterruptedException {
        Map<String, Object> entry = new TreeMap<>();
        entry.put("step", name);
        try {
            action.run();
        } catch (Exception e) {
            entry.put("error", errorOf(e));
        }
        Map<String, List<Object>> listeners = new TreeMap<>();
        for (Object[] e : quiet()) listeners.computeIfAbsent((String) e[0], k -> new ArrayList<>()).add(e[1]);
        entry.put("listeners", listeners);
        out.add(entry);
    }

    private ListenerRegistration query(Executor executor, String name, Query q) {
        return q.addSnapshotListener(executor, (snap, error) -> {
            if (snap == null) {
                record(name, map("error", errorOf(error)));
                return;
            }
            List<Object> docs = new ArrayList<>();
            for (QueryDocumentSnapshot d : snap) docs.add(Arrays.asList(d.getId(), norm(d.getData(), false)));
            List<Object> changes = new ArrayList<>();
            for (DocumentChange c : snap.getDocumentChanges()) {
                changes.add(Arrays.asList(c.getType().name().toLowerCase(), c.getDocument().getId(),
                        (long) c.getOldIndex(), (long) c.getNewIndex()));
            }
            record(name, map("docs", docs, "changes", changes));
        });
    }

    private ListenerRegistration document(Executor executor, String name, DocumentReference ref) {
        return ref.addSnapshotListener(executor, (snap, error) -> {
            if (snap == null) {
                record(name, map("error", errorOf(error)));
                return;
            }
            record(name, map("exists", snap.exists(), "data", snap.exists() ? norm(snap.getData(), false) : null));
        });
    }

    private void listen() throws Exception {
        Executor executor = Executors.newSingleThreadExecutor();
        CollectionReference cities = writer.collection("cities");
        listenStep("seed", () -> {
            WriteBatch batch = writer.batch();
            batch.set(cities.document("SF"), data("pop", 860L, "state", "CA"));
            batch.set(cities.document("LA"), data("pop", 3900L, "state", "CA"));
            batch.set(cities.document("TOK"), data("pop", 9000L, "state", null));
            await(batch.commit());
        });
        List<ListenerRegistration> registrations = new ArrayList<>();
        listenStep("attach listeners", () -> {
            CollectionReference mine = db.collection("cities");
            registrations.add(query(executor, "all", mine));
            registrations.add(query(executor, "pop > 1000", mine.whereGreaterThan("pop", 1000)));
            registrations.add(query(executor, "top 2 by pop", mine.orderBy("pop", Query.Direction.DESCENDING).limit(2)));
            registrations.add(query(executor, "CA by pop", mine.whereEqualTo("state", "CA").orderBy("pop")));
            registrations.add(query(executor, "group landmarks", db.collectionGroup("landmarks")));
            registrations.add(document(executor, "doc SF", db.document("cities/SF")));
            registrations.add(document(executor, "doc NY", db.document("cities/NY")));
            awaitFirst(Arrays.asList("all", "pop > 1000", "top 2 by pop", "CA by pop", "group landmarks",
                    "doc SF", "doc NY"));
        });
        listenStep("add NY (enters several queries)", () -> await(cities.document("NY").set(data("pop", 8300L, "state", "NY"))));
        listenStep("update SF (stays in all)", () -> await(cities.document("SF").update("pop", 870L)));
        listenStep("update LA below 1000 (leaves pop > 1000, moves in order)", () -> await(cities.document("LA").update("pop", 999L)));
        listenStep("update TOK above all (changes the top 2)", () -> await(cities.document("TOK").update("pop", 9500L)));
        listenStep("no-op write", () -> await(cities.document("SF").set(data("pop", 870L, "state", "CA"))));
        listenStep("write to another collection", () -> await(writer.document("other/x").set(data("n", 1L))));
        listenStep("batch: delete TOK, add BJ, change NY state", () -> {
            WriteBatch batch = writer.batch();
            batch.delete(cities.document("TOK"));
            batch.set(cities.document("BJ"), data("pop", 21500L, "state", null));
            batch.update(cities.document("NY"), "state", "CA");
            await(batch.commit());
        });
        listenStep("add a landmark", () -> await(writer.document("cities/SF/landmarks/gg").set(data("name", "Golden Gate"))));
        listenStep("server timestamp", () -> await(cities.document("SF").update("at", FieldValue.serverTimestamp())));
        listenStep("idle for 95 seconds", () -> Thread.sleep(95_000));
        listenStep("update SF after the idle time", () -> await(cities.document("SF").update("pop", 880L)));
        listenStep("delete NY", () -> await(cities.document("NY").delete()));
        listenStep("detach", () -> {
            for (ListenerRegistration registration : registrations) registration.remove();
        });
    }
}
