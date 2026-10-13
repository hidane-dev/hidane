// cloud.google.com/go/firestore against hidane, started on its own.
//
// With FIRESTORE_EMULATOR_HOST set, the client talks to the emulator at that address, with no
// credentials. Start hidane first (`hidane --host 127.0.0.1 --port 8080`), then run
// `FIRESTORE_EMULATOR_HOST=127.0.0.1:8080 go test ./...`.
package example

import (
	"context"
	"net/http"
	"os"
	"slices"
	"testing"
	"time"

	"cloud.google.com/go/firestore"
)

// A `demo-` project needs no Google Cloud project or credentials.
const project = "demo-hidane"

// newClient empties the database, so that each test starts from nothing, and returns a client.
func newClient(t *testing.T) *firestore.Client {
	t.Helper()
	host := os.Getenv("FIRESTORE_EMULATOR_HOST")
	if host == "" {
		t.Fatal("set FIRESTORE_EMULATOR_HOST to hidane's address, such as 127.0.0.1:8080")
	}
	// The emulator's own endpoint deletes every document.
	url := "http://" + host + "/emulator/v1/projects/" + project + "/databases/(default)/documents"
	req, err := http.NewRequest(http.MethodDelete, url, nil)
	if err != nil {
		t.Fatal(err)
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("clearing the database: %s", resp.Status)
	}

	client, err := firestore.NewClient(context.Background(), project)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { client.Close() })
	return client
}

func TestWritesAndReadsADocument(t *testing.T) {
	ctx := context.Background()
	client := newClient(t)
	ref := client.Doc("cities/tokyo")
	_, err := ref.Set(ctx, map[string]any{
		"name": "Tokyo", "population": 14_000_000, "updated": firestore.ServerTimestamp,
	})
	if err != nil {
		t.Fatal(err)
	}

	snapshot, err := ref.Get(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if name := snapshot.Data()["name"]; name != "Tokyo" {
		t.Errorf("name = %v, want Tokyo", name)
	}
	if updated, ok := snapshot.Data()["updated"].(time.Time); !ok || updated.IsZero() {
		t.Errorf("updated = %v, want the server's time", snapshot.Data()["updated"])
	}
}

func TestQueriesOrdersAndCounts(t *testing.T) {
	ctx := context.Background()
	client := newClient(t)
	cities := client.Collection("cities")
	for id, population := range map[string]int{"tokyo": 14_000_000, "osaka": 2_700_000, "nara": 350_000} {
		if _, err := cities.Doc(id).Set(ctx, map[string]any{"population": population}); err != nil {
			t.Fatal(err)
		}
	}

	large := cities.Where("population", ">", 1_000_000)
	docs, err := large.OrderBy("population", firestore.Desc).Documents(ctx).GetAll()
	if err != nil {
		t.Fatal(err)
	}
	var ids []string
	for _, doc := range docs {
		ids = append(ids, doc.Ref.ID)
	}
	if want := []string{"tokyo", "osaka"}; !slices.Equal(ids, want) {
		t.Errorf("documents = %v, want %v", ids, want)
	}

	result, err := large.NewAggregationQuery().WithCount("count").Get(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if count := result.Data()["count"]; count != int64(2) {
		t.Errorf("count = %v, want 2", count)
	}
}

func TestIncrementsACounterInATransaction(t *testing.T) {
	ctx := context.Background()
	client := newClient(t)
	ref := client.Doc("counters/visits")
	if _, err := ref.Set(ctx, map[string]any{"n": 41}); err != nil {
		t.Fatal(err)
	}

	err := client.RunTransaction(ctx, func(ctx context.Context, tx *firestore.Transaction) error {
		snapshot, err := tx.Get(ref)
		if err != nil {
			return err
		}
		return tx.Update(ref, []firestore.Update{{Path: "n", Value: snapshot.Data()["n"].(int64) + 1}})
	})
	if err != nil {
		t.Fatal(err)
	}
	snapshot, err := ref.Get(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if n := snapshot.Data()["n"]; n != int64(42) {
		t.Errorf("n = %v, want 42", n)
	}
}

func TestAListenerReceivesTheDocumentsItsQueryMatches(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	client := newClient(t)
	messages := client.Collection("messages")
	iter := messages.Where("room", "==", "general").Snapshots(ctx)
	defer iter.Stop()

	for _, m := range []map[string]any{
		{"room": "general", "text": "hello"},
		{"room": "random", "text": "elsewhere"},
		{"room": "general", "text": "world"},
	} {
		if _, _, err := messages.Add(ctx, m); err != nil {
			t.Fatal(err)
		}
	}
	for {
		snapshot, err := iter.Next()
		if err != nil {
			t.Fatalf("the listener did not receive both documents: %v", err)
		}
		if snapshot.Size == 2 {
			docs, err := snapshot.Documents.GetAll()
			if err != nil {
				t.Fatal(err)
			}
			var texts []string
			for _, doc := range docs {
				texts = append(texts, doc.Data()["text"].(string))
			}
			slices.Sort(texts)
			if want := []string{"hello", "world"}; !slices.Equal(texts, want) {
				t.Errorf("texts = %v, want %v", texts, want)
			}
			return
		}
	}
}
