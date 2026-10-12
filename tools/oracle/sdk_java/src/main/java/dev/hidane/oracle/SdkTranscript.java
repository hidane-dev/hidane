package dev.hidane.oracle;

import com.google.api.core.ApiFuture;
import com.google.api.gax.rpc.ApiException;
import com.google.api.gax.rpc.ApiStreamObserver;
import com.google.cloud.Timestamp;
import com.google.cloud.firestore.AggregateField;
import com.google.cloud.firestore.AggregateQuery;
import com.google.cloud.firestore.AggregateQuerySnapshot;
import com.google.cloud.firestore.Blob;
import com.google.cloud.firestore.BulkWriter;
import com.google.cloud.firestore.BulkWriterException;
import com.google.cloud.firestore.CollectionReference;
import com.google.cloud.firestore.DocumentChange;
import com.google.cloud.firestore.DocumentReference;
import com.google.cloud.firestore.DocumentSnapshot;
import com.google.cloud.firestore.ExplainOptions;
import com.google.cloud.firestore.ExplainResults;
import com.google.cloud.firestore.FieldMask;
import com.google.cloud.firestore.FieldPath;
import com.google.cloud.firestore.FieldValue;
import com.google.cloud.firestore.Filter;
import com.google.cloud.firestore.Firestore;
import com.google.cloud.firestore.FirestoreException;
import com.google.cloud.firestore.FirestoreOptions;
import com.google.cloud.firestore.GeoPoint;
import com.google.cloud.firestore.ListenerRegistration;
import com.google.cloud.firestore.Precondition;
import com.google.cloud.firestore.Query;
import com.google.cloud.firestore.Query.Direction;
import com.google.cloud.firestore.QueryDocumentSnapshot;
import com.google.cloud.firestore.QuerySnapshot;
import com.google.cloud.firestore.SetOptions;
import com.google.cloud.firestore.Transaction;
import com.google.cloud.firestore.TransactionOptions;
import com.google.cloud.firestore.VectorQuery;
import com.google.cloud.firestore.VectorQueryOptions;
import com.google.cloud.firestore.VectorValue;
import com.google.cloud.firestore.WriteBatch;
import com.google.cloud.firestore.WriteResult;
import io.grpc.StatusRuntimeException;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Base64;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.atomic.AtomicInteger;

/**
 * Runs one section of steps through java-firestore (the Java server SDK) and prints a transcript
 * without times or generated IDs, so the transcripts of two emulators can be diffed.
 * FIRESTORE_EMULATOR_HOST selects the emulator; start it fresh for each section.
 *
 * <p>Usage: {@code mvn -q compile exec:java -Dexec.args=documents|queries|transactions|listen}
 *
 * <p>The sections and their step names are the same in tools/oracle/sdk_go/ and
 * tools/oracle/sdk_python/; tools/oracle/README.md says how to run them.
 */
public final class SdkTranscript {
  private static Firestore db;
  private static final List<Object> out = new ArrayList<>();
  private static final ExecutorService pool = Executors.newCachedThreadPool();

  interface Body {
    Object run() throws Exception;
  }

  interface TxBody {
    Object run(Transaction tx) throws Exception;
  }

  public static void main(String[] args) throws Exception {
    List<String> sections = List.of("documents", "queries", "transactions", "listen");
    if (args.length != 1 || !sections.contains(args[0])) {
      System.err.println("usage: SdkTranscript documents|queries|transactions|listen");
      System.exit(2);
    }
    db = FirestoreOptions.newBuilder().setProjectId("demo-java-" + args[0]).build().getService();
    switch (args[0]) {
      case "documents" -> documents();
      case "queries" -> queries();
      case "transactions" -> transactions();
      default -> listen();
    }
    StringBuilder sb = new StringBuilder();
    json(out, sb, 0);
    System.out.println(sb);
    System.out.flush();
    // Listener and transaction threads can outlive the client; do not wait for them.
    System.exit(0);
  }

  // --- output -------------------------------------------------------------------------------

  /** A map with sorted keys (null values allowed), as the other SDKs print theirs. */
  static Map<String, Object> obj(Object... kv) {
    Map<String, Object> m = new TreeMap<>();
    for (int i = 0; i < kv.length; i += 2) {
      m.put((String) kv[i], kv[i + 1]);
    }
    return m;
  }

  static String mask(String message) {
    return message == null ? null : message.replaceAll("\\d{10,}", "<n>");
  }

  /** A server error as its gRPC code and message; anything else as a client error. */
  static Map<String, Object> errorOf(Throwable e) {
    while ((e instanceof ExecutionException || e instanceof java.util.concurrent.CompletionException)
        && e.getCause() != null) {
      e = e.getCause();
    }
    for (Throwable t = e; t != null; t = t.getCause()) {
      if (t instanceof StatusRuntimeException s) {
        return obj("code", s.getStatus().getCode().name(), "message", mask(s.getStatus().getDescription()));
      }
    }
    if (e instanceof ApiException a) {
      return obj("code", a.getStatusCode().getCode().name(), "message", mask(a.getMessage()));
    }
    if (e instanceof FirestoreException f && f.getStatus() != null) {
      return obj("code", f.getStatus().getCode().name(), "message", mask(f.getStatus().getDescription()));
    }
    return obj("client", mask(e.getMessage()));
  }

  /**
   * A Firestore value as JSON: NaN and infinities as strings, timestamps as RFC 3339 (or
   * "timestamp" when times is false), the other non-JSON types as tagged objects.
   */
  static Object norm(Object v, boolean times) {
    if (v == null || v instanceof Boolean || v instanceof String || v instanceof Long) {
      return v;
    }
    if (v instanceof Integer i) {
      return (long) i;
    }
    if (v instanceof Double d) {
      if (d.isNaN()) {
        return "NaN";
      }
      if (d.isInfinite()) {
        return d > 0 ? "Infinity" : "-Infinity";
      }
      return d;
    }
    if (v instanceof Timestamp t) {
      return times ? t.toString() : "timestamp";
    }
    if (v instanceof Blob b) {
      return obj("bytes", Base64.getEncoder().encodeToString(b.toBytes()));
    }
    if (v instanceof GeoPoint g) {
      return obj("geopoint", List.of(g.getLatitude(), g.getLongitude()));
    }
    if (v instanceof DocumentReference r) {
      return obj("reference", r.getPath());
    }
    if (v instanceof VectorValue vv) {
      List<Object> l = new ArrayList<>();
      for (double d : vv.toArray()) {
        l.add(norm(d, times));
      }
      return obj("vector", l);
    }
    if (v instanceof Map<?, ?> m) {
      Map<String, Object> sorted = new TreeMap<>();
      for (Map.Entry<?, ?> e : m.entrySet()) {
        sorted.put((String) e.getKey(), norm(e.getValue(), times));
      }
      return sorted;
    }
    if (v instanceof List<?> l) {
      List<Object> n = new ArrayList<>();
      for (Object e : l) {
        n.add(norm(e, times));
      }
      return n;
    }
    return obj("unknown", v.getClass().getName() + " " + v);
  }

  static Object norm(Object v) {
    return norm(v, true);
  }

  static void json(Object v, StringBuilder sb, int indent) {
    if (v == null) {
      sb.append("null");
    } else if (v instanceof String s) {
      sb.append('"');
      for (int i = 0; i < s.length(); i++) {
        char c = s.charAt(i);
        switch (c) {
          case '"' -> sb.append("\\\"");
          case '\\' -> sb.append("\\\\");
          case '\n' -> sb.append("\\n");
          case '\r' -> sb.append("\\r");
          case '\t' -> sb.append("\\t");
          default -> {
            if (c < 0x20) {
              sb.append(String.format("\\u%04x", (int) c));
            } else {
              sb.append(c);
            }
          }
        }
      }
      sb.append('"');
    } else if (v instanceof Number || v instanceof Boolean) {
      sb.append(v);
    } else if (v instanceof Map<?, ?> m) {
      if (m.isEmpty()) {
        sb.append("{}");
        return;
      }
      sb.append("{\n");
      int i = 0;
      for (Map.Entry<?, ?> e : m.entrySet()) {
        sb.append(" ".repeat(indent + 1));
        json(e.getKey(), sb, indent + 1);
        sb.append(": ");
        json(e.getValue(), sb, indent + 1);
        sb.append(++i < m.size() ? ",\n" : "\n");
      }
      sb.append(" ".repeat(indent)).append('}');
    } else if (v instanceof List<?> l) {
      if (l.isEmpty()) {
        sb.append("[]");
        return;
      }
      sb.append("[\n");
      for (int i = 0; i < l.size(); i++) {
        sb.append(" ".repeat(indent + 1));
        json(l.get(i), sb, indent + 1);
        sb.append(i + 1 < l.size() ? ",\n" : "\n");
      }
      sb.append(" ".repeat(indent)).append(']');
    } else {
      throw new IllegalArgumentException("not JSON: " + v.getClass());
    }
  }

  static void step(String name, Body body) {
    Map<String, Object> entry = new LinkedHashMap<>();
    entry.put("name", name);
    try {
      entry.put("ok", norm(body.run()));
    } catch (Exception e) {
      entry.put("error", errorOf(e));
    }
    out.add(entry);
  }

  static Object data(DocumentReference ref) throws Exception {
    return norm(ref.get().get().getData());
  }

  static List<String> ids(List<? extends DocumentSnapshot> snaps) {
    List<String> l = new ArrayList<>();
    for (DocumentSnapshot s : snaps) {
      l.add(s.getReference().getPath());
    }
    return l;
  }

  static Body get(Query q) {
    return () -> ids(q.get().get().getDocuments());
  }

  static <T> T await(ApiFuture<T> f) throws Exception {
    return f.get();
  }

  // --- documents ----------------------------------------------------------------------------

  static void documents() {
    CollectionReference users = db.collection("users");
    DocumentReference alice = users.document("alice");
    DocumentReference nobody = users.document("nobody");
    WriteResult[] first = new WriteResult[1];
    step("set alice", () -> {
      first[0] = await(alice.set(obj("name", "Alice", "age", 30, "tags", List.of("a", "b"), "nested", obj("x", 1))));
      return "ok";
    });
    step("get alice", () -> {
      DocumentSnapshot s = await(alice.get());
      return obj("exists", s.exists(), "data", s.getData(),
          "createEqualsUpdate", s.getCreateTime().equals(s.getUpdateTime()),
          "updateEqualsWrite", s.getUpdateTime().equals(first[0].getUpdateTime()));
    });
    step("update dotted", () -> {
      await(alice.update(obj("age", 31, "nested.y", 2)));
      return data(alice);
    });
    step("set merge", () -> {
      await(alice.set(obj("city", "Tokyo", "nested", obj("z", 3)), SetOptions.merge()));
      return data(alice);
    });
    step("set mergeFields", () -> {
      await(alice.set(obj("city", "Osaka", "age", 99), SetOptions.mergeFields("city")));
      return data(alice);
    });
    step("get with a field mask", () -> await(alice.get(FieldMask.of("name", "nested.x"))).getData());
    step("create existing", () -> await(alice.create(obj("x", 1))));
    step("update missing", () -> await(nobody.update(obj("x", 1))));
    step("update with a stale lastUpdateTime",
        () -> await(alice.update(obj("age", 32), Precondition.updatedAt(first[0].getUpdateTime()))));
    step("delete with a stale lastUpdateTime",
        () -> await(alice.delete(Precondition.updatedAt(first[0].getUpdateTime()))));
    step("update with the current updateTime", () -> {
      Timestamp current = await(alice.get()).getUpdateTime();
      await(alice.update(obj("age", 32), Precondition.updatedAt(current)));
      return data(alice);
    });
    step("delete a missing document", () -> {
      await(nobody.delete());
      return "ok";
    });
    // java-firestore has no public `exists` precondition for deletes.
    step("identical set keeps updateTime", () -> {
      DocumentSnapshot before = await(alice.get());
      WriteResult w = await(alice.set(before.getData()));
      DocumentSnapshot after = await(alice.get());
      return obj("writeTimeIsOldUpdateTime", w.getUpdateTime().equals(before.getUpdateTime()),
          "updateTimeUnchanged", after.getUpdateTime().equals(before.getUpdateTime()));
    });
    step("getAll", () -> {
      List<Object> l = new ArrayList<>();
      for (DocumentSnapshot s : await(db.getAll(nobody, alice))) {
        l.add(obj("id", s.getId(), "exists", s.exists()));
      }
      return l;
    });
    step("batch", () -> {
      WriteBatch b = db.batch();
      b.set(users.document("bob"), obj("n", 1));
      b.set(users.document("carol"), obj("n", 2));
      b.delete(alice);
      b.set(db.document("users/bob/posts/p1"), obj("title", "hi"));
      b.set(db.document("ghosts/g/items/i"), obj("n", 3));
      return (long) await(b.commit()).size();
    });
    step("a failing batch writes nothing", () -> {
      WriteBatch b = db.batch();
      b.set(users.document("eve"), obj("n", 1));
      b.update(nobody, obj("x", 1));
      Object commit;
      try {
        await(b.commit());
        commit = "ok";
      } catch (Exception e) {
        commit = errorOf(e);
      }
      return obj("commit", commit, "eveExists", await(users.document("eve").get()).exists());
    });
    step("listDocuments users", () -> refIds(users.listDocuments()));
    step("listDocuments ghosts (missing parent)", () -> refIds(db.collection("ghosts").listDocuments()));
    step("listCollections root", () -> collectionIds(db.listCollections()));
    step("listCollections bob", () -> collectionIds(users.document("bob").listCollections()));
    step("bulkWriter", () -> {
      BulkWriter bw = db.bulkWriter();
      bw.addWriteErrorListener(e -> false);
      List<ApiFuture<WriteResult>> ops = List.of(
          bw.create(users.document("dave"), obj("n", 1)),
          bw.create(users.document("bob"), obj("n", 9)),
          bw.update(nobody, obj("n", 1)),
          bw.delete(users.document("carol")));
      bw.close();
      List<Object> l = new ArrayList<>();
      for (ApiFuture<WriteResult> op : ops) {
        try {
          op.get();
          l.add("ok");
        } catch (ExecutionException e) {
          l.add(e.getCause() instanceof BulkWriterException b
              ? "error " + b.getStatus().getCode().name() : "error " + errorOf(e).get("code"));
        }
      }
      return l;
    });
    step("after bulkWriter", () -> {
      List<String> l = refIds(users.listDocuments());
      Collections.sort(l);
      return l;
    });
    step("add with a generated ID", () -> {
      DocumentReference ref = await(db.collection("auto").add(obj("n", 1)));
      return obj("idLength", (long) ref.getId().length(), "data", data(ref));
    });
    step("serverTimestamp", () -> {
      DocumentReference ref = users.document("ts");
      WriteResult w = await(ref.set(obj("n", 1, "at", FieldValue.serverTimestamp(),
          "nested", obj("at", FieldValue.serverTimestamp()))));
      DocumentSnapshot s = await(ref.get());
      Object at = s.get("at");
      Timestamp t = (Timestamp) at;
      return obj("isTimestamp", at instanceof Timestamp,
          "millisecondPrecision", t.getNanos() % 1_000_000 == 0,
          "notAfterWrite", t.compareTo(w.getUpdateTime()) <= 0,
          "sameInNested", t.equals(s.get("nested.at")));
    });
    step("increment", update(users.document("counter"), obj("n", 1, "f", 1.5), obj(
        "n", FieldValue.increment(2), "f", FieldValue.increment(1), "fresh", FieldValue.increment(5))));
    step("maximum and minimum", update(users.document("bounds"), obj("a", 5, "b", 5, "c", 1.5), obj(
        "a", FieldValue.maximum(7), "b", FieldValue.minimum(3), "c", FieldValue.maximum(1),
        "fresh", FieldValue.minimum(2))));
    step("arrayUnion and arrayRemove", () -> {
      DocumentReference ref = users.document("tags");
      await(ref.set(obj("tags", List.of("a", 1))));
      await(ref.update(obj("tags", FieldValue.arrayUnion("b", "a", 1.0))));
      Object afterUnion = await(ref.get()).get("tags");
      await(ref.update(obj("tags", FieldValue.arrayRemove("a", 1))));
      return obj("afterUnion", afterUnion, "afterRemove", await(ref.get()).get("tags"));
    });
    step("delete a field", update(users.document("del"), obj("keep", 1, "drop", 2, "m", obj("x", 1, "y", 2)),
        obj("drop", FieldValue.delete(), "m.x", FieldValue.delete())));
    step("merge with transforms", () -> {
      DocumentReference ref = users.document("merge");
      await(ref.set(obj("a", 1, "list", List.of("x"))));
      await(ref.set(obj("b", FieldValue.increment(3), "list", FieldValue.arrayUnion("y")), SetOptions.merge()));
      return data(ref);
    });
    step("update quoted field paths", () -> {
      DocumentReference ref = users.document("paths");
      await(ref.set(obj("a.b", obj("c", 1), "plain", 1)));
      await(ref.update(FieldPath.of("a.b", "c"), 2, FieldPath.of("x y"), 3));
      return data(ref);
    });
    step("value types", () -> {
      DocumentReference ref = users.document("types");
      await(ref.set(obj(
          "null", null, "true", true, "int", Long.MAX_VALUE, "negative", -42,
          "double", 0.1, "nan", Double.NaN, "inf", Double.POSITIVE_INFINITY, "-inf", Double.NEGATIVE_INFINITY,
          "string", "h\u00e9llo \u2603 \uD83D\uDE00", "bytes", Blob.fromBytes(new byte[] {0, 1, 2, (byte) 255}),
          "timestamp", Timestamp.ofTimeSecondsAndNanos(1704164645L, 123456000),
          "geopoint", new GeoPoint(35.68, 139.76), "reference", db.document("users/bob"),
          "array", Arrays.asList(1, "a", null, obj("m", true)), "map", obj("a", obj("b", obj("c", 1))),
          "emptyMap", obj(), "emptyArray", List.of(), "vector", FieldValue.vector(new double[] {1.0, 2.5}))));
      return data(ref);
    });
  }

  static Body update(DocumentReference ref, Map<String, Object> initial, Map<String, Object> updates) {
    return () -> {
      await(ref.set(initial));
      await(ref.update(updates));
      return data(ref);
    };
  }

  static List<String> refIds(Iterable<DocumentReference> refs) {
    List<String> l = new ArrayList<>();
    for (DocumentReference r : refs) {
      l.add(r.getId());
    }
    return l;
  }

  static List<String> collectionIds(Iterable<CollectionReference> cols) {
    List<String> l = new ArrayList<>();
    for (CollectionReference c : cols) {
      l.add(c.getId());
    }
    return l;
  }

  // --- queries ------------------------------------------------------------------------------

  static void queries() {
    CollectionReference cities = db.collection("cities");
    step("seed", () -> {
      WriteBatch b = db.batch();
      Map<String, Map<String, Object>> rows = new LinkedHashMap<>();
      rows.put("SF", obj("name", "San Francisco", "state", "CA", "country", "USA", "capital", false,
          "population", 860000, "regions", List.of("west_coast", "norcal"), "loc", obj("lat", 37.7)));
      rows.put("LA", obj("name", "Los Angeles", "state", "CA", "country", "USA", "capital", false,
          "population", 3900000, "regions", List.of("west_coast", "socal"), "loc", obj("lat", 34.0)));
      rows.put("DC", obj("name", "Washington, D.C.", "state", null, "country", "USA", "capital", true,
          "population", 680000, "regions", List.of("east_coast")));
      rows.put("TOK", obj("name", "Tokyo", "state", null, "country", "Japan", "capital", true,
          "population", 9000000, "regions", List.of("kanto", "honshu"), "loc", obj("lat", 35.7)));
      rows.put("BJ", obj("name", "Beijing", "state", null, "country", "China", "capital", true,
          "population", 21500000, "regions", List.of("jingjinji", "hebei")));
      rows.put("NAN", obj("name", "Nowhere", "country", "None", "population", Double.NaN));
      rows.forEach((id, row) -> b.set(cities.document(id), row));
      b.set(db.document("cities/SF/landmarks/golden-gate"), obj("name", "Golden Gate Bridge", "type", "bridge"));
      b.set(db.document("cities/SF/landmarks/legion"), obj("name", "Legion of Honor", "type", "museum"));
      b.set(db.document("cities/TOK/landmarks/national"), obj("name", "National Museum", "type", "museum"));
      b.set(db.document("cities/BJ/landmarks/forbidden"), obj("name", "Forbidden City", "type", "museum"));
      CollectionReference orders = db.collection("orders");
      b.set(orders.document("a"), obj("total", 10, "qty", 1, "status", "paid"));
      b.set(orders.document("b"), obj("total", 2.5, "qty", 3, "status", "paid"));
      b.set(orders.document("c"), obj("total", 7, "qty", 2, "status", "open"));
      b.set(orders.document("d"), obj("total", "n/a", "qty", 1, "status", "open"));
      b.set(orders.document("e"), obj("qty", 5, "status", "void"));
      b.set(db.document("orders/a/lines/1"), obj("qty", 4));
      b.set(db.document("orders/b/lines/1"), obj("qty", 6));
      b.set(db.document("big/x"), obj("n", 9007199254740991L));
      b.set(db.document("big/y"), obj("n", 9007199254740991L));
      Map<String, double[]> vectors = Map.of("a", new double[] {1, 0}, "b", new double[] {0, 1},
          "c", new double[] {1, 1}, "d", new double[] {3, 4});
      vectors.forEach((id, v) -> b.set(db.document("vectors/" + id), obj("v", FieldValue.vector(v), "k", id)));
      await(b.commit());
      return "ok";
    });
    step("all", get(cities));
    step("where ==", get(cities.whereEqualTo("state", "CA")));
    step("where == null (IS_NULL)", get(cities.whereEqualTo("state", null)));
    step("where != null (IS_NOT_NULL)", get(cities.whereNotEqualTo("state", null)));
    step("where == NaN (IS_NAN)", get(cities.whereEqualTo("population", Double.NaN)));
    step("where <", get(cities.whereLessThan("population", 1000000)));
    step("where >= orderBy desc", get(cities.whereGreaterThanOrEqualTo("population", 1000000)
        .orderBy("population", Direction.DESCENDING)));
    step("where !=", get(cities.whereNotEqualTo("country", "USA")));
    step("where in", get(cities.whereIn("country", List.of("Japan", "China"))));
    step("where not-in", get(cities.whereNotIn("country", List.of("USA", "Japan"))));
    step("array-contains", get(cities.whereArrayContains("regions", "west_coast")));
    step("array-contains-any", get(cities.whereArrayContainsAny("regions", List.of("west_coast", "kanto"))));
    step("nested field", get(cities.whereGreaterThan("loc.lat", 35)));
    step("two inequalities", get(cities.whereGreaterThan("population", 700000).whereLessThan("name", "U")));
    step("or", get(cities.where(Filter.or(Filter.equalTo("capital", true), Filter.equalTo("state", "CA")))));
    step("or of and", get(cities.where(Filter.or(
        Filter.and(Filter.equalTo("state", "CA"), Filter.greaterThan("population", 1000000)),
        Filter.equalTo("country", "Japan")))));
    step("orderBy limit", get(cities.orderBy("name").limit(3)));
    step("limitToLast", get(cities.orderBy("name").limitToLast(2)));
    step("offset", get(cities.orderBy("population").offset(2).limit(2)));
    step("startAt / endBefore values", get(cities.orderBy("population").startAt(860000).endBefore(9000000)));
    step("startAfter a snapshot", () -> get(cities.orderBy("country").startAfter(await(cities.document("SF").get()))).run());
    step("paging with startAfter", () -> {
      List<Object> pages = new ArrayList<>();
      Query q = cities.orderBy("population").limit(2);
      while (true) {
        List<QueryDocumentSnapshot> snaps = await(q.get()).getDocuments();
        if (snaps.isEmpty()) {
          return pages;
        }
        pages.add(ids(snaps));
        q = cities.orderBy("population").startAfter(snaps.get(snaps.size() - 1)).limit(2);
      }
    });
    step("select", () -> {
      List<Object> l = new ArrayList<>();
      for (QueryDocumentSnapshot s : await(cities.whereEqualTo("capital", true).select("name", "loc.lat").get())) {
        l.add(obj("id", s.getId(), "data", s.getData()));
      }
      return l;
    });
    step("documentId in", get(cities.whereIn(FieldPath.documentId(),
        List.of(cities.document("TOK"), cities.document("SF")))));
    step("documentId range", get(cities.whereGreaterThanOrEqualTo(FieldPath.documentId(), cities.document("L"))
        .whereLessThan(FieldPath.documentId(), cities.document("T"))));
    step("orderBy documentId desc", get(cities.orderBy(FieldPath.documentId(), Direction.DESCENDING)));
    step("collectionGroup", get(db.collectionGroup("landmarks").whereEqualTo("type", "museum")));
    step("collectionGroup orderBy name", get(db.collectionGroup("landmarks").orderBy("name")));
    step("subcollection", get(cities.document("SF").collection("landmarks")));
    step("stream", () -> {
      List<String> got = Collections.synchronizedList(new ArrayList<>());
      CompletableFuture<Object> done = new CompletableFuture<>();
      cities.whereEqualTo("capital", true).stream(new ApiStreamObserver<>() {
        @Override
        public void onNext(DocumentSnapshot s) {
          got.add(s.getId());
        }

        @Override
        public void onError(Throwable t) {
          done.completeExceptionally(t);
        }

        @Override
        public void onCompleted() {
          done.complete(got);
        }
      });
      return done.get();
    });
    List<Object> in31 = new ArrayList<>();
    for (int i = 0; i < 31; i++) {
      in31.add(i);
    }
    step("in with 31 values", get(cities.whereIn("population", in31)));
    step("explain", () -> {
      ExplainResults<QuerySnapshot> r = await(cities.whereEqualTo("capital", true)
          .explain(ExplainOptions.builder().setAnalyze(true).build()));
      return obj("docs", ids(r.getSnapshot().getDocuments()), "metrics", r.getMetrics() == null ? null : "present");
    });
    step("query in a transaction", () -> await(db.runTransaction(tx -> {
      List<String> result = ids(tx.get(cities.whereEqualTo("capital", true)).get().getDocuments());
      tx.update(cities.document("DC"), "visited", true);
      return result;
    }, readWrite())));
    step("read-only transaction query", () -> await(db.runTransaction(
        tx -> ids(tx.get(cities.whereEqualTo("state", "CA")).get().getDocuments()), readOnly())));
    step("partition query", () -> (long) await(db.collectionGroup("landmarks").getPartitions(2)).size());

    CollectionReference orders = db.collection("orders");
    Query paid = orders.whereEqualTo("status", "paid");
    AggregateField total = AggregateField.sum("total");
    AggregateField mean = AggregateField.average("total");
    AggregateField qty = AggregateField.sum("qty");
    step("count", () -> obj("count", await(orders.count().get()).getCount()));
    step("count with a filter", () -> obj("count", await(orders.whereEqualTo("status", "open").count().get()).getCount()));
    step("count with a limit", () -> obj("count", await(orders.limit(2).count().get()).getCount()));
    step("sum and average", () -> {
      AggregateQuerySnapshot s = await(orders.aggregate(total, mean).get());
      return obj("total", s.get(total), "mean", s.get(mean));
    });
    step("count next to sum", () -> {
      AggregateQuerySnapshot s = await(orders.aggregate(AggregateField.count(), qty).get());
      return obj("n", s.getCount(), "qty", s.get(qty));
    });
    step("average of nothing", () -> {
      AggregateQuerySnapshot s = await(orders.whereEqualTo("status", "none").aggregate(mean, total).get());
      return obj("mean", s.get(mean), "total", s.get(total));
    });
    step("sum beyond 2^53", () -> {
      AggregateField n = AggregateField.sum("n");
      return obj("n", await(db.collection("big").aggregate(n).get()).get(n));
    });
    step("collection group count and sum", () -> {
      AggregateQuerySnapshot s = await(db.collectionGroup("lines").aggregate(AggregateField.count(), qty).get());
      return obj("n", s.getCount(), "qty", s.get(qty));
    });
    step("count in a transaction", () -> await(db.runTransaction(tx -> {
      AggregateQuery q = paid.count();
      Object result = obj("count", tx.get(q).get().getCount());
      tx.set(orders.document("f"), obj("total", 1, "qty", 1, "status", "paid"));
      return result;
    }, readWrite())));
    step("count after the transaction", () -> obj("count", await(paid.count().get()).getCount()));
    for (VectorQuery.DistanceMeasure measure : List.of(VectorQuery.DistanceMeasure.EUCLIDEAN,
        VectorQuery.DistanceMeasure.DOT_PRODUCT)) {
      String name = measure == VectorQuery.DistanceMeasure.EUCLIDEAN ? "euclidean" : "dot product";
      step("findNearest " + name, () -> {
        List<Object> l = new ArrayList<>();
        for (DocumentSnapshot s : await(db.collection("vectors").findNearest("v", new double[] {1.0, 0.5}, 3, measure,
            VectorQueryOptions.newBuilder().setDistanceResultField("distance").build()).get()).getDocuments()) {
          l.add(List.of(s.getId(), s.get("distance")));
        }
        return l;
      });
    }
    step("recursiveDelete a document", () -> {
      await(db.recursiveDelete(cities.document("SF")));
      return obj("cities", get(cities).run(), "landmarks", get(db.collectionGroup("landmarks")).run());
    });
    step("recursiveDelete a collection", () -> {
      await(db.recursiveDelete(cities));
      return obj("cities", get(cities).run(), "landmarks", get(db.collectionGroup("landmarks")).run());
    });
  }

  static TransactionOptions readWrite() {
    return TransactionOptions.createReadWriteOptionsBuilder().setExecutor(pool).build();
  }

  static TransactionOptions readOnly() {
    return TransactionOptions.createReadOnlyOptionsBuilder().setExecutor(pool).build();
  }

  // --- transactions -------------------------------------------------------------------------

  /** Runs body in a transaction and counts how many attempts it took. */
  static Map<String, Object> attempts(TxBody body, TransactionOptions options) throws Exception {
    AtomicInteger n = new AtomicInteger();
    Object result = await(db.runTransaction(tx -> {
      n.incrementAndGet();
      try {
        return body.run(tx);
      } catch (RuntimeException e) {
        throw e;
      } catch (Exception e) {
        throw new RuntimeException(e);
      }
    }, options));
    return obj("attempts", (long) n.get(), "result", result);
  }

  static Map<String, Object> attempts(TxBody body) throws Exception {
    return attempts(body, readWrite());
  }

  static Map<String, Object> with(Map<String, Object> o, String key, DocumentReference ref) throws Exception {
    o.put(key, data(ref));
    return o;
  }

  static void transactions() {
    CollectionReference c = db.collection("txn");
    DocumentReference a = c.document("a");
    step("create when missing", () -> with(attempts(tx -> {
      DocumentSnapshot s = tx.get(a).get();
      if (!s.exists()) {
        tx.set(a, obj("n", 0));
      }
      return s.exists();
    }), "data", a));
    step("read-modify-write", () -> with(attempts(tx -> {
      long n = tx.get(a).get().getLong("n");
      tx.update(a, "n", n + 1);
      return n;
    }), "data", a));
    step("getAll", () -> attempts(tx -> {
      List<Object> l = new ArrayList<>();
      for (DocumentSnapshot s : tx.getAll(a, c.document("missing")).get()) {
        l.add(obj("id", s.getId(), "exists", s.exists()));
      }
      return l;
    }));
    step("write-only transaction", () -> with(attempts(tx -> {
      tx.set(c.document("b"), obj("n", 1));
      tx.delete(c.document("nothing"));
      return null;
    }), "data", c.document("b")));
    Timestamp[] snapshotTime = new Timestamp[1];
    step("read-only", () -> {
      snapshotTime[0] = await(a.get()).getReadTime();
      return attempts(tx -> norm(tx.get(a).get().getData()), readOnly());
    });
    step("read-only at a past read time", () -> {
      await(a.update("n", 100));
      return attempts(tx -> norm(tx.get(a).get().getData()), TransactionOptions.createReadOnlyOptionsBuilder()
          .setExecutor(pool).setReadTime(snapshotTime[0].toProto()).build());
    });
    step("read-only cannot write", () -> attempts(tx -> {
      tx.set(c.document("ro"), obj("n", 1));
      return null;
    }, readOnly()));
    step("a failing callback rolls back and releases its locks", () -> {
      String failed = null;
      try {
        attempts(tx -> {
          tx.get(a).get();
          throw new IllegalStateException("boom");
        });
      } catch (Exception e) {
        Throwable t = e;
        while (t.getCause() != null) {
          t = t.getCause();
        }
        failed = t.getMessage();
      }
      long start = System.nanoTime();
      await(a.set(obj("n", 1)));
      return obj("failed", failed, "outsideWriteWaited", System.nanoTime() - start > 1_000_000_000L);
    });
    step("create of an existing document", () -> attempts(tx -> {
      tx.get(c.document("b")).get();
      tx.create(c.document("b"), obj("n", 2));
      return null;
    }));
    step("an outside write waits for the transaction that read the document", () -> {
      DocumentReference ref = c.document("w");
      CompletableFuture<Object> txn = holding(ref, 500);
      long start = System.nanoTime();
      Object outside = outsideWrite(ref);
      boolean waited = System.nanoTime() - start > 300_000_000L;
      return with(obj("outside", outside, "outsideWaited", waited, "transaction", txn.get()), "final", ref);
    });
    step("an outside write gives up after the lock timeout", () -> {
      DocumentReference ref = c.document("t");
      CompletableFuture<Object> txn = holding(ref, 3000);
      Object outside = outsideWrite(ref);
      return with(obj("outside", outside, "transaction", txn.get()), "final", ref);
    });
    step("two transactions increment the same counter", () -> {
      DocumentReference ref = c.document("counter");
      await(ref.set(obj("n", 0)));
      // Both read the counter, then commit 300 ms apart: each commit waits for the other's
      // lock, the first wait times out after 2 s and ends the deadlock, one transaction is
      // retried. (Commits at the same instant can both time out, which makes the number of
      // attempts depend on the SDK's backoff.)
      List<CompletableFuture<Map<String, Object>>> runs = new ArrayList<>();
      for (long delay : new long[] {100, 400}) {
        TxBody inc = tx -> {
          long n = tx.get(ref).get().getLong("n");
          Thread.sleep(delay);
          tx.update(ref, "n", n + 1);
          return null;
        };
        runs.add(CompletableFuture.supplyAsync(() -> {
          try {
            return attempts(inc);
          } catch (Exception e) {
            throw new RuntimeException(e);
          }
        }, pool));
      }
      long total = 0;
      for (CompletableFuture<Map<String, Object>> r : runs) {
        total += (Long) r.get().get("attempts");
      }
      return with(obj("totalAttempts", total), "final", ref);
    });
  }

  /**
   * Runs a transaction that reads ref, holds it for the given time and writes it; returns once
   * it has read, with its outcome to come.
   */
  static CompletableFuture<Object> holding(DocumentReference ref, long millis) throws Exception {
    CountDownLatch reading = new CountDownLatch(1);
    CompletableFuture<Object> outcome = CompletableFuture.supplyAsync(() -> {
      try {
        return attempts(tx -> {
          tx.get(ref).get();
          reading.countDown();
          Thread.sleep(millis);
          tx.set(ref, obj("by", "transaction"));
          return null;
        });
      } catch (Exception e) {
        return errorOf(e);
      }
    }, pool);
    reading.await();
    return outcome;
  }

  static Object outsideWrite(DocumentReference ref) {
    try {
      await(ref.set(obj("by", "outside")));
      return "ok";
    } catch (Exception e) {
      return errorOf(e);
    }
  }

  // --- listen -------------------------------------------------------------------------------

  private static final Object lock = new Object();
  private static long last = System.nanoTime();
  private static final List<Map<String, Object>> steps = new ArrayList<>();

  @SuppressWarnings("unchecked")
  static void record(String listener, Object entry) {
    synchronized (lock) {
      Map<String, List<Object>> listeners =
          (Map<String, List<Object>>) steps.get(steps.size() - 1).get("listeners");
      listeners.computeIfAbsent(listener, k -> new ArrayList<>()).add(entry);
      last = System.nanoTime();
    }
  }

  static void quiet() throws InterruptedException {
    Thread.sleep(150);
    while (true) {
      synchronized (lock) {
        if (System.nanoTime() - last >= 300_000_000L) {
          return;
        }
      }
      Thread.sleep(50);
    }
  }

  static void section(String name, Body body) {
    synchronized (lock) {
      Map<String, Object> s = new LinkedHashMap<>();
      s.put("step", name);
      s.put("listeners", new TreeMap<String, List<Object>>());
      steps.add(s);
    }
    try {
      body.run();
      quiet();
    } catch (Exception e) {
      record("step", errorOf(e));
    }
  }

  static ListenerRegistration queryListener(String name, Query q) {
    return q.addSnapshotListener(pool, (snap, error) -> {
      if (error != null) {
        record(name, obj("error", errorOf(error)));
        return;
      }
      List<Object> docs = new ArrayList<>();
      for (QueryDocumentSnapshot d : snap.getDocuments()) {
        docs.add(List.of(d.getId(), norm(d.getData(), false)));
      }
      List<Object> changes = new ArrayList<>();
      for (DocumentChange ch : snap.getDocumentChanges()) {
        changes.add(List.of(ch.getType().name().toLowerCase(), ch.getDocument().getId(),
            (long) ch.getOldIndex(), (long) ch.getNewIndex()));
      }
      record(name, obj("docs", docs, "changes", changes));
    });
  }

  static ListenerRegistration docListener(String name, DocumentReference ref) {
    return ref.addSnapshotListener(pool, (snap, error) -> {
      if (error != null) {
        record(name, obj("error", errorOf(error)));
        return;
      }
      record(name, obj("exists", snap.exists(), "data", snap.exists() ? norm(snap.getData(), false) : null));
    });
  }

  static void listen() {
    CollectionReference c = db.collection("cities");
    section("seed", () -> {
      WriteBatch b = db.batch();
      b.set(c.document("SF"), obj("pop", 860, "state", "CA"));
      b.set(c.document("LA"), obj("pop", 3900, "state", "CA"));
      b.set(c.document("TOK"), obj("pop", 9000, "state", null));
      return await(b.commit());
    });
    List<ListenerRegistration> registrations = new ArrayList<>();
    section("attach listeners", () -> {
      registrations.add(queryListener("all", c));
      registrations.add(queryListener("pop > 1000", c.whereGreaterThan("pop", 1000)));
      registrations.add(queryListener("CA by pop", c.whereEqualTo("state", "CA").orderBy("pop")));
      registrations.add(queryListener("top 2 by pop", c.orderBy("pop", Direction.DESCENDING).limit(2)));
      registrations.add(docListener("doc SF", c.document("SF")));
      registrations.add(docListener("doc NY", c.document("NY")));
      registrations.add(queryListener("group landmarks", db.collectionGroup("landmarks")));
      return null;
    });
    section("add NY (enters several queries)", () -> await(c.document("NY").set(obj("pop", 8300, "state", "NY"))));
    section("update SF (stays in all)", () -> await(c.document("SF").update("pop", 870)));
    section("update LA below 1000 (leaves pop > 1000, moves in order)", () -> await(c.document("LA").update("pop", 999)));
    section("update TOK above all (changes the top 2)", () -> await(c.document("TOK").update("pop", 9500)));
    section("no-op write", () -> await(c.document("SF").set(obj("pop", 870, "state", "CA"))));
    section("write to another collection", () -> await(db.document("other/x").set(obj("n", 1))));
    section("batch: delete TOK, add BJ, change NY state", () -> {
      WriteBatch b = db.batch();
      b.delete(c.document("TOK"));
      b.set(c.document("BJ"), obj("pop", 21500, "state", null));
      b.update(c.document("NY"), "state", "CA");
      return await(b.commit());
    });
    section("add a landmark", () -> await(db.document("cities/SF/landmarks/gg").set(obj("name", "Golden Gate"))));
    section("server timestamp", () -> await(c.document("SF").update("at", FieldValue.serverTimestamp())));
    section("delete NY", () -> await(c.document("NY").delete()));
    section("detach", () -> {
      for (ListenerRegistration r : registrations) {
        r.remove();
      }
      return null;
    });
    // Listeners fire in no particular order: within each step the snapshots are grouped by
    // listener (sorted by name), each listener's own order kept.
    out.addAll(steps);
  }

  private SdkTranscript() {}
}
