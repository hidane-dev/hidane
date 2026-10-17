// hidane's container image through Testcontainers for Go (docs/testcontainers.md): in place of the
// Cloud SDK image in modules/gcloud/firestore, which starts it with
// `/bin/sh -c "gcloud beta emulators firestore start --host-port 0.0.0.0:8080 --project=<id>"` and
// waits for the port and a log line with "running", and as a generic container. HIDANE_IMAGE names
// the image to test; the module needs hidane 0.5.0 or newer (`-run TestGenericContainer` for
// older ones).
package hidane

import (
	"context"
	"io"
	"os"
	"reflect"
	"strings"
	"testing"
	"time"

	"cloud.google.com/go/datastore"
	"cloud.google.com/go/firestore"
	"github.com/testcontainers/testcontainers-go"
	tcfirestore "github.com/testcontainers/testcontainers-go/modules/gcloud/firestore"
	"github.com/testcontainers/testcontainers-go/wait"
	"google.golang.org/api/option"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/status"
)

// A `demo-` project needs no Google Cloud project or credentials.
const project = "demo-testcontainers"

// image is the image under test: an image of hidane, such as ghcr.io/hidane-dev/hidane:<version>.
func image(t *testing.T) string {
	t.Helper()
	image := os.Getenv("HIDANE_IMAGE")
	if image == "" {
		t.Fatal("set HIDANE_IMAGE to the hidane image to test, such as ghcr.io/hidane-dev/hidane:<version>")
	}
	return image
}

// ownerToken is the emulator's owner credential, which the module's own example sends.
type ownerToken struct{}

func (ownerToken) GetRequestMetadata(context.Context, ...string) (map[string]string, error) {
	return map[string]string{"authorization": "Bearer owner"}, nil
}

func (ownerToken) RequireTransportSecurity() bool { return false }

func TestFirestoreModule(t *testing.T) {
	ctx := context.Background()
	ctr, err := tcfirestore.Run(ctx, image(t), tcfirestore.WithProjectID(project))
	testcontainers.CleanupContainer(t, ctr)
	if err != nil {
		t.Fatal(err)
	}

	// hidane says on stderr which of its own flags it serves the gcloud command line with.
	logs, err := ctr.Logs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	text, err := io.ReadAll(logs)
	logs.Close()
	if err != nil {
		t.Fatal(err)
	}
	note := "in place of `gcloud emulators firestore start`, with: hidane --host 0.0.0.0 --port 8080"
	if !strings.Contains(string(text), note) {
		t.Errorf("the logs do not say %q:\n%s", note, text)
	}

	t.Run("FIRESTORE_EMULATOR_HOST", func(t *testing.T) {
		// The variable also tells the client not to look for credentials.
		t.Setenv("FIRESTORE_EMULATOR_HOST", ctr.URI())
		client, err := firestore.NewClient(ctx, ctr.ProjectID())
		if err != nil {
			t.Fatal(err)
		}
		run(t, client)
	})

	t.Run("gRPC connection", func(t *testing.T) {
		// As the module's own example connects.
		conn, err := grpc.NewClient(ctr.URI(),
			grpc.WithTransportCredentials(insecure.NewCredentials()),
			grpc.WithPerRPCCredentials(ownerToken{}))
		if err != nil {
			t.Fatal(err)
		}
		client, err := firestore.NewClient(ctx, ctr.ProjectID(), option.WithGRPCConn(conn))
		if err != nil {
			t.Fatal(err)
		}
		run(t, client)
	})
}

// The module's WithDatastoreMode() (#81): the Datastore API through the client library of
// Datastore, and the Firestore API refused, as the official emulator does in that mode. Served from
// the release after 0.6.0 on: HIDANE_DATASTORE says the image under test serves it.
func TestFirestoreModuleInDatastoreMode(t *testing.T) {
	if os.Getenv("HIDANE_DATASTORE") == "" {
		t.Skip("set HIDANE_DATASTORE when the image serves Datastore mode")
	}
	ctx := context.Background()
	ctr, err := tcfirestore.Run(ctx, image(t), tcfirestore.WithProjectID(project),
		tcfirestore.WithDatastoreMode())
	testcontainers.CleanupContainer(t, ctr)
	if err != nil {
		t.Fatal(err)
	}
	t.Setenv("DATASTORE_EMULATOR_HOST", ctr.URI())
	client, err := datastore.NewClient(ctx, ctr.ProjectID())
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	type task struct {
		Text string
		Done bool
	}
	key := datastore.NameKey("Task", "t1", nil)
	if _, err := client.Put(ctx, key, &task{Text: "a"}); err != nil {
		t.Fatal(err)
	}
	var got task
	if err := client.Get(ctx, key, &got); err != nil || got != (task{Text: "a"}) {
		t.Fatalf("get: %v %+v", err, got)
	}
	keys, err := client.GetAll(ctx, datastore.NewQuery("Task").FilterField("Done", "=", false).KeysOnly(), nil)
	if err != nil || len(keys) != 1 || keys[0].Name != "t1" {
		t.Fatalf("query: %v %v", err, keys)
	}

	t.Setenv("FIRESTORE_EMULATOR_HOST", ctr.URI())
	fs, err := firestore.NewClient(ctx, ctr.ProjectID())
	if err != nil {
		t.Fatal(err)
	}
	defer fs.Close()
	_, err = fs.Doc("c/d").Get(ctx)
	if status.Code(err) != codes.InvalidArgument || !strings.Contains(err.Error(), "Database mode not set to Firestore Mode") {
		t.Fatalf("the Firestore API in Datastore mode: %v", err)
	}
}

func TestGenericContainer(t *testing.T) {
	ctx := context.Background()
	ctr, err := testcontainers.Run(ctx, image(t),
		testcontainers.WithExposedPorts("8080/tcp"),
		testcontainers.WithWaitStrategy(wait.ForLog("Dev App Server is now running.")),
	)
	testcontainers.CleanupContainer(t, ctr)
	if err != nil {
		t.Fatal(err)
	}
	endpoint, err := ctr.PortEndpoint(ctx, "8080/tcp", "")
	if err != nil {
		t.Fatal(err)
	}

	// FIRESTORE_EMULATOR_HOST also tells the client not to look for credentials.
	t.Setenv("FIRESTORE_EMULATOR_HOST", endpoint)
	client, err := firestore.NewClient(ctx, project)
	if err != nil {
		t.Fatal(err)
	}
	run(t, client)
}

// run writes, reads, queries, runs transactions, writes in bulk and listens to a query, then closes
// the client. It sets every document it reads first, so it can run again on the same emulator.
func run(t *testing.T, client *firestore.Client) {
	t.Helper()
	defer client.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()

	users := client.Collection("users")
	people := map[string]map[string]any{
		"alovelace": {"first": "Ada", "last": "Lovelace", "born": 1815},
		"aturing":   {"first": "Alan", "last": "Turing", "born": 1912},
		"ghopper":   {"first": "Grace", "last": "Hopper", "born": 1906},
	}
	for id, data := range people {
		if _, err := users.Doc(id).Set(ctx, data); err != nil {
			t.Fatal(err)
		}
	}

	ada, err := users.Doc("alovelace").Get(ctx)
	if err != nil {
		t.Fatal(err)
	}
	want := map[string]any{"first": "Ada", "last": "Lovelace", "born": int64(1815)}
	if got := ada.Data(); !reflect.DeepEqual(got, want) {
		t.Errorf("alovelace = %v, want %v", got, want)
	}

	modern, err := users.Where("born", ">", 1900).OrderBy("born", firestore.Asc).Documents(ctx).GetAll()
	if err != nil {
		t.Fatal(err)
	}
	if got := ids(modern); !reflect.DeepEqual(got, []string{"ghopper", "aturing"}) {
		t.Errorf("born after 1900 = %v, want [ghopper aturing]", got)
	}

	counter := client.Collection("counters").Doc("visits")
	if _, err := counter.Set(ctx, map[string]any{"count": 0}); err != nil {
		t.Fatal(err)
	}
	for range 3 {
		err := client.RunTransaction(ctx, func(ctx context.Context, tx *firestore.Transaction) error {
			snapshot, err := tx.Get(counter)
			if err != nil {
				return err
			}
			count, err := snapshot.DataAt("count")
			if err != nil {
				return err
			}
			return tx.Update(counter, []firestore.Update{{Path: "count", Value: count.(int64) + 1}})
		})
		if err != nil {
			t.Fatal(err)
		}
	}
	if _, err := counter.Update(ctx, []firestore.Update{{Path: "count", Value: firestore.Increment(10)}}); err != nil {
		t.Fatal(err)
	}
	snapshot, err := counter.Get(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if count := snapshot.Data()["count"]; count != int64(13) {
		t.Errorf("count = %v, want 13", count)
	}

	// The Go client's replacement for write batches: BulkWriter, over BatchWrite.
	bulk := client.BulkWriter(ctx)
	deleted, err := bulk.Delete(users.Doc("aturing"))
	if err != nil {
		t.Fatal(err)
	}
	updated, err := bulk.Update(users.Doc("ghopper"), []firestore.Update{{Path: "rank", Value: "Rear Admiral"}})
	if err != nil {
		t.Fatal(err)
	}
	bulk.End()
	for _, job := range []*firestore.BulkWriterJob{deleted, updated} {
		if _, err := job.Results(); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := users.Doc("aturing").Get(ctx); status.Code(err) != codes.NotFound {
		t.Errorf("aturing after the bulk delete: %v, want NotFound", err)
	}

	listener := users.OrderBy("born", firestore.Desc).Snapshots(ctx)
	defer listener.Stop()
	first, err := listener.Next()
	if err != nil {
		t.Fatal(err)
	}
	seen, err := first.Documents.GetAll()
	if err != nil {
		t.Fatal(err)
	}
	if got := ids(seen); !reflect.DeepEqual(got, []string{"ghopper", "alovelace"}) {
		t.Errorf("the listener saw %v, want [ghopper alovelace]", got)
	}
}

func ids(docs []*firestore.DocumentSnapshot) []string {
	out := make([]string, len(docs))
	for i, doc := range docs {
		out[i] = doc.Ref.ID
	}
	return out
}
