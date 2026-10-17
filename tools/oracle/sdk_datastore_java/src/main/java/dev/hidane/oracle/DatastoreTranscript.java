package dev.hidane.oracle;

import com.google.cloud.NoCredentials;
import com.google.cloud.Timestamp;
import com.google.cloud.datastore.AggregationQuery;
import com.google.cloud.datastore.AggregationResult;
import com.google.cloud.datastore.AggregationResults;
import com.google.cloud.datastore.BaseEntity;
import com.google.cloud.datastore.Blob;
import com.google.cloud.datastore.BlobValue;
import com.google.cloud.datastore.BooleanValue;
import com.google.cloud.datastore.Cursor;
import com.google.cloud.datastore.Datastore;
import com.google.cloud.datastore.DatastoreException;
import com.google.cloud.datastore.DatastoreOptions;
import com.google.cloud.datastore.DoubleValue;
import com.google.cloud.datastore.Entity;
import com.google.cloud.datastore.EntityValue;
import com.google.cloud.datastore.FullEntity;
import com.google.cloud.datastore.GqlQuery;
import com.google.cloud.datastore.IncompleteKey;
import com.google.cloud.datastore.Key;
import com.google.cloud.datastore.KeyFactory;
import com.google.cloud.datastore.KeyValue;
import com.google.cloud.datastore.LatLng;
import com.google.cloud.datastore.LatLngValue;
import com.google.cloud.datastore.ListValue;
import com.google.cloud.datastore.LongValue;
import com.google.cloud.datastore.NullValue;
import com.google.cloud.datastore.PathElement;
import com.google.cloud.datastore.ProjectionEntity;
import com.google.cloud.datastore.Query;
import com.google.cloud.datastore.QueryResults;
import com.google.cloud.datastore.StringValue;
import com.google.cloud.datastore.StructuredQuery.CompositeFilter;
import com.google.cloud.datastore.StructuredQuery.OrderBy;
import com.google.cloud.datastore.StructuredQuery.PropertyFilter;
import com.google.cloud.datastore.TimestampValue;
import com.google.cloud.datastore.Transaction;
import com.google.cloud.datastore.Value;
import com.google.cloud.datastore.aggregation.Aggregation;
import com.google.cloud.grpc.GrpcTransportOptions;
import com.google.cloud.http.HttpTransportOptions;
import com.google.datastore.v1.TransactionOptions;
import java.util.ArrayList;
import java.util.Base64;
import java.util.Iterator;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;
import java.util.concurrent.Callable;

/**
 * Runs one section of steps through java-datastore (the Java client library of Datastore) and
 * prints a transcript, so the transcripts of two emulators in Datastore mode can be diffed (#81).
 * DATASTORE_EMULATOR_HOST selects the emulator, started with {@code --database-mode
 * datastore-mode}; start it fresh for each section. The client speaks gRPC (its default), or
 * HTTP (REST with protobuf bodies) when TRANSPORT is {@code http}.
 *
 * <p>Usage: mvn -q compile exec:java -Dexec.args=entities|queries|transactions
 */
public final class DatastoreTranscript {
  private static String project;
  private static Datastore db;
  private static final List<Object> out = new ArrayList<>();

  interface Step {
    Object run() throws Exception;
  }

  public static void main(String[] args) throws Exception {
    String section = args.length == 1 ? args[0] : "";
    if (!List.of("entities", "queries", "transactions").contains(section)) {
      System.err.println("Usage: DatastoreTranscript entities|queries|transactions");
      System.exit(2);
    }
    project = "demo-datastore-java-" + section;
    db = client(null);
    switch (section) {
      case "entities" -> entities();
      case "queries" -> queries();
      default -> transactions();
    }
    StringBuilder json = new StringBuilder();
    write(json, out, 0);
    System.out.println(json);
    System.exit(0);
  }

  private static Datastore client(String namespace) {
    DatastoreOptions.Builder options =
        DatastoreOptions.newBuilder()
            .setProjectId(project)
            .setHost(System.getenv("DATASTORE_EMULATOR_HOST"))
            .setCredentials(NoCredentials.getInstance());
    if (namespace != null) {
      options.setNamespace(namespace);
    }
    if ("http".equals(System.getenv("TRANSPORT"))) {
      options.setTransportOptions(HttpTransportOptions.newBuilder().build());
    } else {
      options.setTransportOptions(GrpcTransportOptions.newBuilder().build());
    }
    return options.build().getService();
  }

  // --- the transcript ---------------------------------------------------------------------------

  private static String mask(String message) {
    return message == null ? null : message.replace(project, "{project}").replaceAll("\\d{10,}", "<n>");
  }

  private static void step(String name, Step step) {
    Map<String, Object> entry = new LinkedHashMap<>();
    entry.put("name", name);
    try {
      entry.put("ok", step.run());
    } catch (DatastoreException e) {
      Map<String, Object> error = new LinkedHashMap<>();
      error.put("code", e.getReason());
      error.put("message", mask(e.getMessage()));
      entry.put("error", error);
    } catch (Exception e) {
      entry.put("error", Map.of("client", e.getClass().getSimpleName() + ": " + mask(e.getMessage())));
    }
    out.add(entry);
  }

  private static Object keyOf(Key key) {
    List<Object> path = new ArrayList<>();
    for (PathElement element : key.getAncestors()) {
      path.add(element.getKind());
      path.add(element.hasId() ? element.getId() : element.getName());
    }
    path.add(key.getKind());
    path.add(key.hasId() ? key.getId() : key.getName());
    Map<String, Object> out = new LinkedHashMap<>();
    out.put("path", path);
    if (!key.getNamespace().isEmpty()) {
      out.put("namespace", key.getNamespace());
    }
    return out;
  }

  private static Object norm(Value<?> value) {
    if (value instanceof NullValue) {
      return null;
    }
    if (value instanceof StringValue s) {
      return s.get();
    }
    if (value instanceof LongValue l) {
      return l.get();
    }
    if (value instanceof BooleanValue b) {
      return b.get();
    }
    if (value instanceof DoubleValue d) {
      double v = d.get();
      if (Double.isNaN(v)) {
        return "NaN";
      }
      if (Double.isInfinite(v)) {
        return v > 0 ? "Infinity" : "-Infinity";
      }
      return v;
    }
    if (value instanceof TimestampValue t) {
      return t.get().toString();
    }
    if (value instanceof BlobValue b) {
      return Map.of("bytes", Base64.getEncoder().encodeToString(b.get().toByteArray()));
    }
    if (value instanceof LatLngValue l) {
      return Map.of("geopoint", List.of(l.get().getLatitude(), l.get().getLongitude()));
    }
    if (value instanceof KeyValue k) {
      return Map.of("key", keyOf(k.get()));
    }
    if (value instanceof EntityValue e) {
      return entity(e.get());
    }
    if (value instanceof ListValue l) {
      List<Object> items = new ArrayList<>();
      for (Value<?> item : l.get()) {
        items.add(norm(item));
      }
      return items;
    }
    return Map.of("unknown", String.valueOf(value));
  }

  private static Object entity(BaseEntity<?> entity) {
    if (entity == null) {
      return null;
    }
    Map<String, Object> props = new TreeMap<>();
    for (String name : entity.getNames()) {
      props.put(name, norm(entity.getValue(name)));
    }
    Map<String, Object> out = new LinkedHashMap<>();
    out.put("properties", props);
    if (entity.getKey() instanceof Key key) {
      out.put("key", keyOf(key));
    }
    return out;
  }

  private static <T> List<Object> keys(Iterator<T> results) {
    List<Object> out = new ArrayList<>();
    while (results.hasNext()) {
      T next = results.next();
      if (next instanceof Key key) {
        out.add(keyOf(key));
      } else if (next instanceof Entity e) {
        out.add(keyOf(e.getKey()));
      } else if (next instanceof ProjectionEntity p) {
        out.add(entity(p));
      }
    }
    return out;
  }

  // --- entities ---------------------------------------------------------------------------------

  private static void entities() {
    KeyFactory tasks = db.newKeyFactory().setKind("Task");
    Key task = tasks.newKey("t1");
    Entity every =
        Entity.newBuilder(task)
            .set("text", "a")
            .set("count", 3)
            .set("ratio", 0.5)
            .set("done", false)
            .setNull("nothing")
            .set("at", Timestamp.ofTimeSecondsAndNanos(1577934245L, 678000000))
            .set("raw", Blob.copyFrom(new byte[] {0, 1}))
            .set("place", LatLng.of(35.0, 139.0))
            .set("owner", db.newKeyFactory().setKind("User").newKey("u1"))
            .set("tags", "x", "y")
            .set("empty", ListValue.newBuilder().build())
            .set("nested", FullEntity.newBuilder().set("x", 1).set("y", "z").build())
            .set("long", StringValue.newBuilder("b".repeat(2000)).setExcludeFromIndexes(true).build())
            .set("big", 1L << 62)
            .set("nan", Double.NaN)
            .set("inf", Double.POSITIVE_INFINITY)
            .set("ninf", Double.NEGATIVE_INFINITY)
            .set("zero", -0.0)
            .build();
    step("put every type", () -> {
      db.put(every);
      return "ok";
    });
    step("get it back", () -> entity(db.get(task)));
    step("get a missing entity", () -> entity(db.get(tasks.newKey("nobody"))));
    step("get several, one missing", () -> {
      List<Object> found = new ArrayList<>();
      for (Entity e : db.fetch(task, tasks.newKey("nobody"))) {
        found.add(e == null ? null : keyOf(e.getKey()));
      }
      return found;
    });
    step("overwrite it", () -> {
      db.put(Entity.newBuilder(task).set("text", "b").build());
      return entity(db.get(task));
    });
    step("put an incomplete key", () -> {
      Entity e = db.add(FullEntity.newBuilder(tasks.newKey()).set("text", "generated").build());
      Map<String, Object> out = new LinkedHashMap<>();
      out.put("id", e.getKey().getId());
      out.put("read", entity(db.get(e.getKey())));
      return out;
    });
    step("allocate three IDs", () -> {
      List<Object> ids = new ArrayList<>();
      for (Key k : db.allocateId(tasks.newKey(), tasks.newKey(), tasks.newKey())) {
        ids.add(k.getId());
      }
      return ids;
    });
    step("allocate an ID under a parent", () -> keyOf(db.allocateId(
        db.newKeyFactory().addAncestor(PathElement.of("List", "l1")).setKind("Task").newKey())));
    step("reserve an ID", () -> {
      db.reserveIds(tasks.newKey(5));
      return "ok";
    });
    Key child = db.newKeyFactory().addAncestor(PathElement.of("List", "l1")).setKind("Task").newKey("c1");
    step("put a child", () -> {
      db.put(Entity.newBuilder(child).set("text", "child").build());
      return entity(db.get(child));
    });
    step("get a child's parent, never written", () -> entity(db.get(child.getParent())));
    Datastore other = client("ns1");
    step("put in a namespace", () -> {
      Key k = other.newKeyFactory().setKind("Task").newKey("t1");
      other.put(Entity.newBuilder(k).set("text", "ns").build());
      return entity(other.get(k));
    });
    step("the default namespace keeps its own", () -> db.get(task).getString("text"));
    KeyFactory many = db.newKeyFactory().setKind("Many");
    step("put several", () -> {
      db.put(Entity.newBuilder(many.newKey(1)).set("i", 1).build(),
          Entity.newBuilder(many.newKey(2)).set("i", 2).build(),
          Entity.newBuilder(many.newKey(3)).set("i", 3).build());
      return keys(db.get(many.newKey(1), many.newKey(2), many.newKey(3)));
    });
    step("delete several", () -> {
      db.delete(many.newKey(1), many.newKey(2));
      return keys(db.get(many.newKey(1), many.newKey(2), many.newKey(3)));
    });
    step("delete a missing entity", () -> {
      db.delete(tasks.newKey("nobody"));
      return "ok";
    });
    step("delete it", () -> {
      db.delete(task);
      return entity(db.get(task));
    });
    step("insert an existing entity", () -> {
      db.put(Entity.newBuilder(tasks.newKey("dup")).set("a", 1).build());
      return entity(db.add(Entity.newBuilder(tasks.newKey("dup")).set("a", 2).build()));
    });
    step("update a missing entity", () -> {
      db.update(Entity.newBuilder(tasks.newKey("nobody")).set("a", 1).build());
      return "ok";
    });
    step("an indexed string of 1501 bytes",
        () -> entity(db.put(Entity.newBuilder(tasks.newKey("long")).set("text", "a".repeat(1501)).build())));
    step("an indexed string of 1500 bytes", () -> {
      db.put(Entity.newBuilder(tasks.newKey("long")).set("text", "a".repeat(1500)).build());
      return "ok";
    });
    step("a property named __x__",
        () -> entity(db.put(Entity.newBuilder(tasks.newKey("r")).set("__x__", 1).build())));
    step("an entity over 1 MiB", () -> entity(db.put(Entity.newBuilder(tasks.newKey("huge"))
        .set("a", StringValue.newBuilder("a".repeat(1_100_000)).setExcludeFromIndexes(true).build())
        .build())));
    step("a key name __x__",
        () -> entity(db.put(Entity.newBuilder(tasks.newKey("__x__")).set("a", 1).build())));
    step("a kind __x__", () -> entity(db.put(
        Entity.newBuilder(db.newKeyFactory().setKind("__x__").newKey("a")).set("a", 1).build())));
  }

  // --- queries ----------------------------------------------------------------------------------

  private static void queries() {
    KeyFactory p = db.newKeyFactory().setKind("P");
    Object[][] people = {
      {"p1", 1L, "x", List.of("red", "blue")},
      {"p2", 2L, "y", List.of("red")},
      {"p3", 2L, "x", List.of()},
      {"p4", 3L, "z", List.of("blue", "green")},
      {"p5", 3L, "y", null},
      {"p6", -1L, "x", List.of("green")},
      {"p7", null, "w", null},
      {"p8", 4.5, "x", null},
    };
    for (Object[] person : people) {
      Entity.Builder e = Entity.newBuilder(p.newKey((String) person[0])).set("b", (String) person[2]);
      if (person[1] instanceof Long l) {
        e.set("a", l);
      } else if (person[1] instanceof Double d) {
        e.set("a", d);
      }
      if (person[3] instanceof List<?> tags) {
        ListValue.Builder list = ListValue.newBuilder();
        for (Object tag : tags) {
          list.addValue((String) tag);
        }
        e.set("tags", list.build());
      }
      db.put(e.build());
    }
    Key root = db.newKeyFactory().setKind("F").newKey("root");
    KeyFactory children = db.newKeyFactory().addAncestor(PathElement.of("F", "root")).setKind("C");
    db.put(Entity.newBuilder(root).set("n", 0).build(),
        Entity.newBuilder(children.newKey("c1")).set("n", 1).build(),
        Entity.newBuilder(children.newKey("c2")).set("n", 2).build(),
        Entity.newBuilder(db.newKeyFactory().setKind("C").newKey("c3")).set("n", 3).build());
    Datastore other = client("ns1");
    other.put(Entity.newBuilder(other.newKeyFactory().setKind("P").newKey("n1")).set("a", 1).set("b", "x").build());

    step("kind P", () -> keys(db.run(Query.newEntityQueryBuilder().setKind("P").build())));
    step("a = 2", () -> keys(db.run(Query.newEntityQueryBuilder().setKind("P").setFilter(PropertyFilter.eq("a", 2)).build())));
    step("a < 2", () -> keys(db.run(Query.newEntityQueryBuilder().setKind("P").setFilter(PropertyFilter.lt("a", 2)).build())));
    step("a >= 2", () -> keys(db.run(Query.newEntityQueryBuilder().setKind("P").setFilter(PropertyFilter.ge("a", 2)).build())));
    step("a != 2", () -> keys(db.run(Query.newEntityQueryBuilder().setKind("P").setFilter(PropertyFilter.neq("a", 2)).build())));
    step("a IN [1, 3]", () -> keys(db.run(Query.newEntityQueryBuilder().setKind("P")
        .setFilter(PropertyFilter.in("a", ListValue.of(1, 3))).build())));
    step("a NOT_IN [2, 3]", () -> keys(db.run(Query.newEntityQueryBuilder().setKind("P")
        .setFilter(PropertyFilter.not_in("a", ListValue.of(2, 3))).build())));
    step("a = null", () -> keys(db.run(Query.newEntityQueryBuilder().setKind("P").setFilter(PropertyFilter.isNull("a")).build())));
    step("a = 4.5", () -> keys(db.run(Query.newEntityQueryBuilder().setKind("P").setFilter(PropertyFilter.eq("a", 4.5)).build())));
    step("tags = red", () -> keys(db.run(Query.newEntityQueryBuilder().setKind("P").setFilter(PropertyFilter.eq("tags", "red")).build())));
    step("a = 2 and b = x", () -> keys(db.run(Query.newEntityQueryBuilder().setKind("P")
        .setFilter(CompositeFilter.and(PropertyFilter.eq("a", 2), PropertyFilter.eq("b", "x"))).build())));
    step("a = 1 or b = y", () -> keys(db.run(Query.newEntityQueryBuilder().setKind("P")
        .setFilter(CompositeFilter.or(PropertyFilter.eq("a", 1), PropertyFilter.eq("b", "y"))).build())));
    step("a > 1 and b > w", () -> keys(db.run(Query.newEntityQueryBuilder().setKind("P")
        .setFilter(CompositeFilter.and(PropertyFilter.gt("a", 1), PropertyFilter.gt("b", "w"))).build())));
    step("order by a", () -> keys(db.run(Query.newEntityQueryBuilder().setKind("P").setOrderBy(OrderBy.asc("a")).build())));
    step("order by -a, b", () -> keys(db.run(Query.newEntityQueryBuilder().setKind("P")
        .setOrderBy(OrderBy.desc("a"), OrderBy.asc("b")).build())));
    step("__key__ > p4", () -> keys(db.run(Query.newEntityQueryBuilder().setKind("P")
        .setFilter(PropertyFilter.gt("__key__", p.newKey("p4"))).build())));
    step("keys only", () -> keys(db.run(Query.newKeyQueryBuilder().setKind("P").build())));
    step("project a, b", () -> keys(db.run(Query.newProjectionEntityQueryBuilder().setKind("P")
        .setProjection("a", "b").build())));
    step("distinct on b", () -> keys(db.run(Query.newProjectionEntityQueryBuilder().setKind("P")
        .setProjection("b").setDistinctOn("b").build())));
    step("limit 2, offset 3", () -> keys(db.run(Query.newEntityQueryBuilder().setKind("P").setLimit(2).setOffset(3).build())));
    step("pages of 3 by cursor", () -> {
      List<Object> pages = new ArrayList<>();
      Cursor cursor = null;
      for (int i = 0; i < 5; i++) {
        var q = Query.newEntityQueryBuilder().setKind("P").setOrderBy(OrderBy.asc("b")).setLimit(3);
        if (cursor != null) {
          q.setStartCursor(cursor);
        }
        QueryResults<Entity> results = db.run(q.build());
        List<Object> page = keys(results);
        if (page.isEmpty()) {
          break;
        }
        pages.add(page);
        cursor = results.getCursorAfter();
      }
      return pages;
    });
    step("children of F root", () -> keys(db.run(Query.newEntityQueryBuilder().setKind("C")
        .setFilter(PropertyFilter.hasAncestor(root)).build())));
    step("kind P in ns1", () -> keys(other.run(Query.newEntityQueryBuilder().setKind("P").build())));
    step("kinds", () -> keys(db.run(Query.newKeyQueryBuilder().setKind("__kind__").build())));
    step("GQL a >= 2 order by b", () -> keys(db.run(Query.newGqlQueryBuilder(Query.ResultType.ENTITY,
        "SELECT * FROM P WHERE a >= 2 ORDER BY b").setAllowLiteral(true).build())));
    step("GQL with a binding", () -> keys(db.run(Query.newGqlQueryBuilder(Query.ResultType.ENTITY,
        "SELECT * FROM P WHERE b = @b").setBinding("b", "x").build())));
    step("GQL keys only", () -> keys(db.run(Query.newGqlQueryBuilder(Query.ResultType.KEY,
        "SELECT __key__ FROM P WHERE a > 2").setAllowLiteral(true).build())));
    step("GQL refused", () -> keys(db.run(Query.newGqlQueryBuilder(Query.ResultType.ENTITY,
        "SELECT * FROM P WHERE").build())));
    step("count P", () -> {
      AggregationResults results = db.runAggregation(Query.newAggregationQueryBuilder()
          .over(Query.newEntityQueryBuilder().setKind("P").build())
          .addAggregation(Aggregation.count().as("n")).build());
      return results.get(0).getLong("n");
    });
    step("sum and avg of a", () -> {
      AggregationResult result = db.runAggregation(Query.newAggregationQueryBuilder()
          .over(Query.newEntityQueryBuilder().setKind("P").build())
          .addAggregation(Aggregation.sum("a").as("s"))
          .addAggregation(Aggregation.avg("a").as("m")).build()).get(0);
      Map<String, Object> out = new LinkedHashMap<>();
      out.put("s", result.getDouble("s"));
      out.put("m", result.getDouble("m"));
      return out;
    });
    step("count b = x by GQL", () -> db.runAggregation(Query.newAggregationQueryBuilder()
        .over(Query.newGqlQueryBuilder("AGGREGATE COUNT(*) AS n OVER (SELECT * FROM P WHERE b = 'x')")
            .setAllowLiteral(true).build()).build()).get(0).getLong("n"));
    step("IN with 31 values", () -> {
      ListValue.Builder values = ListValue.newBuilder();
      for (int i = 0; i < 31; i++) {
        values.addValue(i);
      }
      return keys(db.run(Query.newEntityQueryBuilder().setKind("P").setFilter(PropertyFilter.in("a", values.build())).build()));
    });
    step("project a, filter a = 1", () -> keys(db.run(Query.newProjectionEntityQueryBuilder().setKind("P")
        .setProjection("a").setFilter(PropertyFilter.eq("a", 1)).build())));
  }

  // --- transactions -----------------------------------------------------------------------------

  private static void transactions() {
    Key account = db.newKeyFactory().setKind("Account").newKey("a1");
    db.put(Entity.newBuilder(account).set("balance", 100).build());
    step("read and write in a transaction", () -> {
      Transaction tx = db.newTransaction();
      Entity current = tx.get(account);
      tx.put(Entity.newBuilder(current).set("balance", current.getLong("balance") - 30).build());
      tx.commit();
      return db.get(account).getLong("balance");
    });
    step("roll back", () -> {
      Transaction tx = db.newTransaction();
      tx.put(Entity.newBuilder(account).set("balance", 0).build());
      tx.rollback();
      return db.get(account).getLong("balance");
    });
    step("run in a transaction", () -> db.runInTransaction(tx -> {
      Entity current = tx.get(account);
      tx.put(Entity.newBuilder(current).set("balance", current.getLong("balance") + 1).build());
      return current.getLong("balance");
    }));
    step("read only", () -> {
      Transaction tx = db.newTransaction(TransactionOptions.newBuilder()
          .setReadOnly(TransactionOptions.ReadOnly.getDefaultInstance()).build());
      long balance = tx.get(account).getLong("balance");
      tx.commit();
      return balance;
    });
    step("a write in a read-only transaction", () -> {
      Transaction tx = db.newTransaction(TransactionOptions.newBuilder()
          .setReadOnly(TransactionOptions.ReadOnly.getDefaultInstance()).build());
      tx.put(Entity.newBuilder(account).set("balance", 5).build());
      tx.commit();
      return "ok";
    });
    KeyFactory accts = db.newKeyFactory().addAncestor(PathElement.of("Bank", "b1")).setKind("Acct");
    Key bank = db.newKeyFactory().setKind("Bank").newKey("b1");
    for (int i = 1; i <= 3; i++) {
      db.put(Entity.newBuilder(accts.newKey(i)).set("n", i).build());
    }
    step("an ancestor query in a transaction", () -> {
      Transaction tx = db.newTransaction();
      List<Object> ns = new ArrayList<>();
      QueryResults<Entity> results = tx.run(Query.newEntityQueryBuilder().setKind("Acct")
          .setFilter(PropertyFilter.hasAncestor(bank)).build());
      while (results.hasNext()) {
        ns.add(results.next().getLong("n"));
      }
      tx.rollback();
      return ns;
    });
    step("a count in a transaction", () -> {
      Transaction tx = db.newTransaction();
      long n = tx.runAggregation(Query.newAggregationQueryBuilder()
          .over(Query.newEntityQueryBuilder().setKind("Acct").setFilter(PropertyFilter.hasAncestor(bank)).build())
          .addAggregation(Aggregation.count().as("n")).build()).get(0).getLong("n");
      tx.rollback();
      return n;
    });
    step("a missing entity in a transaction", () -> {
      Transaction tx = db.newTransaction();
      Object e = entity(tx.get(db.newKeyFactory().setKind("Account").newKey("nobody")));
      tx.rollback();
      return e;
    });
    step("commit twice", () -> {
      Transaction tx = db.newTransaction();
      tx.put(Entity.newBuilder(account).set("balance", 50).build());
      tx.commit();
      tx.commit();
      return "ok";
    });
    step("after it, the balance", () -> db.get(account).getLong("balance"));
    step("an incomplete key in a transaction", () -> {
      Transaction tx = db.newTransaction();
      tx.putWithDeferredIdAllocation(FullEntity.newBuilder(db.newKeyFactory().setKind("Account").newKey()).set("balance", 1).build());
      Transaction.Response response = tx.commit();
      Key generated = response.getGeneratedKeys().get(0);
      return db.get(generated).getLong("balance");
    });
  }

  // --- JSON -------------------------------------------------------------------------------------

  private static void write(StringBuilder b, Object v, int depth) {
    if (v == null) {
      b.append("null");
    } else if (v instanceof String s) {
      b.append('"');
      for (char c : s.toCharArray()) {
        switch (c) {
          case '"' -> b.append("\\\"");
          case '\\' -> b.append("\\\\");
          case '\n' -> b.append("\\n");
          default -> {
            if (c < 0x20) {
              b.append(String.format("\\u%04x", (int) c));
            } else {
              b.append(c);
            }
          }
        }
      }
      b.append('"');
    } else if (v instanceof Map<?, ?> m) {
      if (m.isEmpty()) {
        b.append("{}");
        return;
      }
      b.append("{\n");
      int i = 0;
      for (Map.Entry<?, ?> e : m.entrySet()) {
        b.append(" ".repeat(depth + 1));
        write(b, String.valueOf(e.getKey()), depth + 1);
        b.append(": ");
        write(b, e.getValue(), depth + 1);
        b.append(++i < m.size() ? ",\n" : "\n");
      }
      b.append(" ".repeat(depth)).append('}');
    } else if (v instanceof List<?> l) {
      if (l.isEmpty()) {
        b.append("[]");
        return;
      }
      b.append("[\n");
      for (int i = 0; i < l.size(); i++) {
        b.append(" ".repeat(depth + 1));
        write(b, l.get(i), depth + 1);
        b.append(i + 1 < l.size() ? ",\n" : "\n");
      }
      b.append(" ".repeat(depth)).append(']');
    } else {
      b.append(v);
    }
  }
}
