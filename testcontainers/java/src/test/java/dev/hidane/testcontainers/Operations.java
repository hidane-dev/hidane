package dev.hidane.testcontainers;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;

import com.google.cloud.NoCredentials;
import com.google.cloud.firestore.CollectionReference;
import com.google.cloud.firestore.DocumentReference;
import com.google.cloud.firestore.DocumentSnapshot;
import com.google.cloud.firestore.FieldValue;
import com.google.cloud.firestore.Firestore;
import com.google.cloud.firestore.FirestoreOptions;
import com.google.cloud.firestore.ListenerRegistration;
import com.google.cloud.firestore.Query;
import com.google.cloud.firestore.QueryDocumentSnapshot;
import com.google.cloud.firestore.QuerySnapshot;
import com.google.cloud.firestore.WriteBatch;
import java.util.List;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.TimeUnit;

/** Firestore operations every test runs against the emulator it started. */
final class Operations {
    private Operations() {}

    /** The image under test: an image of hidane, such as ghcr.io/hidane-dev/hidane:<version>. */
    static String image() {
        String image = System.getenv("HIDANE_IMAGE");
        if (image == null || image.isBlank()) {
            throw new IllegalStateException(
                    "Set HIDANE_IMAGE to the hidane image to test, such as ghcr.io/hidane-dev/hidane:<version>");
        }
        return image;
    }

    /** A client for the emulator at {@code host:port}, as Testcontainers' own example connects. */
    static Firestore client(String endpoint) {
        return FirestoreOptions.getDefaultInstance().toBuilder()
                .setProjectId("demo-testcontainers")
                .setHost(endpoint)
                .setCredentials(NoCredentials.getInstance())
                .build()
                .getService();
    }

    /** Writes, reads, queries, a transaction, a batch and a listener. */
    static void run(Firestore db) throws Exception {
        CollectionReference users = db.collection("users");

        users.document("alovelace").set(Map.of("first", "Ada", "last", "Lovelace", "born", 1815)).get();
        users.document("aturing").set(Map.of("first", "Alan", "last", "Turing", "born", 1912)).get();
        users.document("ghopper").set(Map.of("first", "Grace", "last", "Hopper", "born", 1906)).get();

        DocumentSnapshot ada = users.document("alovelace").get().get();
        assertEquals("Ada", ada.getString("first"));
        assertEquals(1815L, ada.getLong("born"));

        QuerySnapshot modern = users.whereGreaterThan("born", 1900).orderBy("born").get().get();
        assertEquals(
                List.of("ghopper", "aturing"),
                modern.getDocuments().stream().map(QueryDocumentSnapshot::getId).toList());

        DocumentReference counter = db.collection("counters").document("visits");
        counter.set(Map.of("count", 0)).get();
        for (int i = 0; i < 3; i++) {
            db.runTransaction(tx -> {
                long count = tx.get(counter).get().getLong("count");
                tx.update(counter, "count", count + 1);
                return null;
            }).get();
        }
        counter.update("count", FieldValue.increment(10)).get();
        assertEquals(13L, counter.get().get().getLong("count"));

        WriteBatch batch = db.batch();
        batch.delete(users.document("aturing"));
        batch.update(users.document("ghopper"), "rank", "Rear Admiral");
        batch.commit().get();
        assertFalse(users.document("aturing").get().get().exists());

        CompletableFuture<List<String>> seen = new CompletableFuture<>();
        ListenerRegistration listener = users.orderBy("born", Query.Direction.DESCENDING)
                .addSnapshotListener((snapshot, error) -> {
                    if (error != null) {
                        seen.completeExceptionally(error);
                    } else if (snapshot != null) {
                        seen.complete(snapshot.getDocuments().stream()
                                .map(QueryDocumentSnapshot::getId)
                                .toList());
                    }
                });
        try {
            assertEquals(List.of("ghopper", "alovelace"), seen.get(30, TimeUnit.SECONDS));
        } finally {
            listener.remove();
        }
    }
}
