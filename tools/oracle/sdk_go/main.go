// Command sdk_go runs one section of steps through cloud.google.com/go/firestore (the Go server
// SDK) and prints a transcript without times or generated IDs, so the transcripts of two
// emulators can be diffed. FIRESTORE_EMULATOR_HOST selects the emulator; start it fresh for each
// section.
//
// Usage: go run . documents|queries|transactions|listen > transcript.json
//
// The sections and their step names are the same in tools/oracle/sdk_python/ and
// tools/oracle/sdk_java/; tools/oracle/README.md says how to run them.
package main

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"os"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"cloud.google.com/go/firestore"
	"google.golang.org/api/iterator"
	"google.golang.org/genproto/googleapis/type/latlng"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

var (
	ctx    = context.Background()
	client *firestore.Client
	out    []any
)

type obj = map[string]any

func main() {
	sections := map[string]func(){
		"documents": documents, "queries": queries, "transactions": transactions, "listen": listen,
	}
	if len(os.Args) != 2 || sections[os.Args[1]] == nil {
		fmt.Fprintln(os.Stderr, "usage: go run . documents|queries|transactions|listen > transcript.json")
		os.Exit(2)
	}
	var err error
	client, err = firestore.NewClient(ctx, "demo-go-"+os.Args[1])
	if err != nil {
		panic(err)
	}
	sections[os.Args[1]]()
	enc := json.NewEncoder(os.Stdout)
	enc.SetEscapeHTML(false)
	enc.SetIndent("", " ")
	if err := enc.Encode(out); err != nil {
		panic(err)
	}
	client.Close()
}

var digits = regexp.MustCompile(`\d{10,}`)

// codeNames are the canonical names of the gRPC codes, as the other SDKs print them.
var codeNames = map[codes.Code]string{
	codes.OK: "OK", codes.Canceled: "CANCELLED", codes.Unknown: "UNKNOWN",
	codes.InvalidArgument: "INVALID_ARGUMENT", codes.DeadlineExceeded: "DEADLINE_EXCEEDED",
	codes.NotFound: "NOT_FOUND", codes.AlreadyExists: "ALREADY_EXISTS",
	codes.PermissionDenied: "PERMISSION_DENIED", codes.ResourceExhausted: "RESOURCE_EXHAUSTED",
	codes.FailedPrecondition: "FAILED_PRECONDITION", codes.Aborted: "ABORTED",
	codes.OutOfRange: "OUT_OF_RANGE", codes.Unimplemented: "UNIMPLEMENTED",
	codes.Internal: "INTERNAL", codes.Unavailable: "UNAVAILABLE", codes.DataLoss: "DATA_LOSS",
	codes.Unauthenticated: "UNAUTHENTICATED",
}

// errorOf is a server error as its gRPC code and message; anything else as a client error.
func errorOf(err error) obj {
	// errors.As finds the status under the SDK's wrapping, with the server's own message.
	var grpcErr interface{ GRPCStatus() *status.Status }
	if errors.As(err, &grpcErr) {
		s := grpcErr.GRPCStatus()
		return obj{"code": codeNames[s.Code()], "message": digits.ReplaceAllString(s.Message(), "<n>")}
	}
	return obj{"client": digits.ReplaceAllString(err.Error(), "<n>")}
}

// number prints a double so that it stays a double in JSON (`1.0`, not `1`).
func number(f float64) any {
	switch {
	case math.IsNaN(f):
		return "NaN"
	case math.IsInf(f, 1):
		return "Infinity"
	case math.IsInf(f, -1):
		return "-Infinity"
	}
	format := byte('f')
	if a := math.Abs(f); a != 0 && (a < 1e-6 || a >= 1e21) {
		format = 'e'
	}
	s := strconv.FormatFloat(f, format, -1, 64)
	if !strings.ContainsAny(s, ".e") {
		s += ".0"
	}
	return json.Number(s)
}

// norm is a Firestore value as JSON: doubles kept apart from integers, NaN and infinities as
// strings, timestamps as RFC 3339 (or "timestamp" when times is false), the other types tagged.
func norm(v any, times bool) any {
	switch x := v.(type) {
	case nil, bool, string, int, int64:
		return x
	case float64:
		return number(x)
	case time.Time:
		if !times {
			return "timestamp"
		}
		return x.UTC().Format(time.RFC3339Nano)
	case []byte:
		return obj{"bytes": base64.StdEncoding.EncodeToString(x)}
	case *latlng.LatLng:
		return obj{"geopoint": []any{number(x.Latitude), number(x.Longitude)}}
	case *firestore.DocumentRef:
		return obj{"reference": rel(x)}
	case firestore.Vector64:
		vs := []any{}
		for _, f := range x {
			vs = append(vs, number(f))
		}
		return obj{"vector": vs}
	case map[string]any:
		m := obj{}
		for k, e := range x {
			m[k] = norm(e, times)
		}
		return m
	case []any:
		l := []any{}
		for _, e := range x {
			l = append(l, norm(e, times))
		}
		return l
	}
	return obj{"unknown": fmt.Sprintf("%T %v", v, v)}
}

func rel(ref *firestore.DocumentRef) string {
	_, path, _ := strings.Cut(ref.Path, "/documents/")
	return path
}

type okStep struct {
	Name string `json:"name"`
	OK   any    `json:"ok"`
}

type errStep struct {
	Name  string `json:"name"`
	Error obj    `json:"error"`
}

func step(name string, fn func() (any, error)) {
	v, err := fn()
	if err != nil {
		out = append(out, errStep{name, errorOf(err)})
	} else {
		out = append(out, okStep{name, v})
	}
}

func data(ref *firestore.DocumentRef) (any, error) {
	s, err := ref.Get(ctx)
	if err != nil {
		return nil, err
	}
	return norm(s.Data(), true), nil
}

func ids(snaps []*firestore.DocumentSnapshot) []string {
	l := []string{}
	for _, s := range snaps {
		l = append(l, rel(s.Ref))
	}
	return l
}

func docIDs(refs []*firestore.DocumentRef) []string {
	l := []string{}
	for _, r := range refs {
		l = append(l, r.ID)
	}
	return l
}

func get(q firestore.Queryer) func() (any, error) {
	return func() (any, error) {
		snaps, err := query(q).Documents(ctx).GetAll()
		return ids(snaps), err
	}
}

func query(q firestore.Queryer) firestore.Query {
	switch x := q.(type) {
	case firestore.Query:
		return x
	case *firestore.CollectionRef:
		return x.Query
	case *firestore.CollectionGroupRef:
		return x.Query
	}
	panic(fmt.Sprintf("not a query: %T", q))
}

type upd = []firestore.Update

// --- documents ------------------------------------------------------------------------------

func documents() {
	users := client.Collection("users")
	alice := users.Doc("alice")
	nobody := users.Doc("nobody")
	var first *firestore.WriteResult
	step("set alice", func() (any, error) {
		var err error
		first, err = alice.Set(ctx, obj{"name": "Alice", "age": 30, "tags": []any{"a", "b"}, "nested": obj{"x": 1}})
		return "ok", err
	})
	step("get alice", func() (any, error) {
		s, err := alice.Get(ctx)
		if err != nil {
			return nil, err
		}
		return obj{"exists": s.Exists(), "data": norm(s.Data(), true),
			"createEqualsUpdate": s.CreateTime.Equal(s.UpdateTime),
			"updateEqualsWrite":  s.UpdateTime.Equal(first.UpdateTime)}, nil
	})
	step("update dotted", func() (any, error) {
		if _, err := alice.Update(ctx, upd{{Path: "age", Value: 31}, {Path: "nested.y", Value: 2}}); err != nil {
			return nil, err
		}
		return data(alice)
	})
	step("set merge", func() (any, error) {
		if _, err := alice.Set(ctx, obj{"city": "Tokyo", "nested": obj{"z": 3}}, firestore.MergeAll); err != nil {
			return nil, err
		}
		return data(alice)
	})
	step("set mergeFields", func() (any, error) {
		if _, err := alice.Set(ctx, obj{"city": "Osaka", "age": 99}, firestore.Merge([]string{"city"})); err != nil {
			return nil, err
		}
		return data(alice)
	})
	step("create existing", func() (any, error) { _, err := alice.Create(ctx, obj{"x": 1}); return "unexpected", err })
	step("update missing", func() (any, error) {
		_, err := nobody.Update(ctx, upd{{Path: "x", Value: 1}})
		return "unexpected", err
	})
	step("update with a stale lastUpdateTime", func() (any, error) {
		_, err := alice.Update(ctx, upd{{Path: "age", Value: 32}}, firestore.LastUpdateTime(first.UpdateTime))
		return "unexpected", err
	})
	step("delete with a stale lastUpdateTime", func() (any, error) {
		_, err := alice.Delete(ctx, firestore.LastUpdateTime(first.UpdateTime))
		return "unexpected", err
	})
	step("update with the current updateTime", func() (any, error) {
		s, err := alice.Get(ctx)
		if err != nil {
			return nil, err
		}
		if _, err := alice.Update(ctx, upd{{Path: "age", Value: 32}}, firestore.LastUpdateTime(s.UpdateTime)); err != nil {
			return nil, err
		}
		return data(alice)
	})
	step("delete a missing document", func() (any, error) { _, err := nobody.Delete(ctx); return "ok", err })
	step("delete a missing document that must exist", func() (any, error) {
		_, err := nobody.Delete(ctx, firestore.Exists)
		return "unexpected", err
	})
	step("identical set keeps updateTime", func() (any, error) {
		before, err := alice.Get(ctx)
		if err != nil {
			return nil, err
		}
		w, err := alice.Set(ctx, before.Data())
		if err != nil {
			return nil, err
		}
		after, err := alice.Get(ctx)
		if err != nil {
			return nil, err
		}
		return obj{"writeTimeIsOldUpdateTime": w.UpdateTime.Equal(before.UpdateTime),
			"updateTimeUnchanged": after.UpdateTime.Equal(before.UpdateTime)}, nil
	})
	step("getAll", func() (any, error) {
		snaps, err := client.GetAll(ctx, []*firestore.DocumentRef{nobody, alice})
		l := []any{}
		for _, s := range snaps {
			l = append(l, obj{"id": s.Ref.ID, "exists": s.Exists()})
		}
		return l, err
	})
	step("batch", func() (any, error) {
		b := client.Batch()
		b.Set(users.Doc("bob"), obj{"n": 1})
		b.Set(users.Doc("carol"), obj{"n": 2})
		b.Delete(alice)
		b.Set(client.Doc("users/bob/posts/p1"), obj{"title": "hi"})
		b.Set(client.Doc("ghosts/g/items/i"), obj{"n": 3})
		results, err := b.Commit(ctx)
		return len(results), err
	})
	step("a failing batch writes nothing", func() (any, error) {
		b := client.Batch()
		b.Set(users.Doc("eve"), obj{"n": 1})
		b.Update(nobody, upd{{Path: "x", Value: 1}})
		var commit any = "ok"
		if _, err := b.Commit(ctx); err != nil {
			commit = errorOf(err)
		}
		s, err := users.Doc("eve").Get(ctx)
		if status.Code(err) == codes.NotFound {
			err = nil
		}
		return obj{"commit": commit, "eveExists": s != nil && s.Exists()}, err
	})
	step("listDocuments users", func() (any, error) {
		refs, err := users.DocumentRefs(ctx).GetAll()
		return docIDs(refs), err
	})
	step("listDocuments ghosts (missing parent)", func() (any, error) {
		refs, err := client.Collection("ghosts").DocumentRefs(ctx).GetAll()
		return docIDs(refs), err
	})
	collectionIDs := func(it *firestore.CollectionIterator) (any, error) {
		cols, err := it.GetAll()
		l := []string{}
		for _, c := range cols {
			l = append(l, c.ID)
		}
		return l, err
	}
	step("listCollections root", func() (any, error) { return collectionIDs(client.Collections(ctx)) })
	step("listCollections bob", func() (any, error) { return collectionIDs(users.Doc("bob").Collections(ctx)) })
	step("bulkWriter", func() (any, error) {
		bw := client.BulkWriter(ctx)
		var jobs []*firestore.BulkWriterJob
		for _, enqueue := range []func() (*firestore.BulkWriterJob, error){
			func() (*firestore.BulkWriterJob, error) { return bw.Create(users.Doc("dave"), obj{"n": 1}) },
			func() (*firestore.BulkWriterJob, error) { return bw.Create(users.Doc("bob"), obj{"n": 9}) },
			func() (*firestore.BulkWriterJob, error) { return bw.Update(nobody, upd{{Path: "n", Value: 1}}) },
			func() (*firestore.BulkWriterJob, error) { return bw.Delete(users.Doc("carol")) },
		} {
			j, err := enqueue()
			if err != nil {
				return nil, err
			}
			jobs = append(jobs, j)
		}
		bw.End()
		l := []any{}
		for _, j := range jobs {
			if _, err := j.Results(); err != nil {
				l = append(l, "error "+errorOf(err)["code"].(string))
			} else {
				l = append(l, "ok")
			}
		}
		return l, nil
	})
	step("after bulkWriter", func() (any, error) {
		refs, err := users.DocumentRefs(ctx).GetAll()
		l := docIDs(refs)
		sort.Strings(l)
		return l, err
	})
	step("add with a generated ID", func() (any, error) {
		ref, _, err := client.Collection("auto").Add(ctx, obj{"n": 1})
		if err != nil {
			return nil, err
		}
		d, err := data(ref)
		return obj{"idLength": len(ref.ID), "data": d}, err
	})
	step("serverTimestamp", func() (any, error) {
		ref := users.Doc("ts")
		w, err := ref.Set(ctx, obj{"n": 1, "at": firestore.ServerTimestamp, "nested": obj{"at": firestore.ServerTimestamp}})
		if err != nil {
			return nil, err
		}
		s, err := ref.Get(ctx)
		if err != nil {
			return nil, err
		}
		at, isTime := s.Data()["at"].(time.Time)
		nested := s.Data()["nested"].(map[string]any)["at"].(time.Time)
		return obj{"isTimestamp": isTime, "millisecondPrecision": at.Nanosecond()%1_000_000 == 0,
			"notAfterWrite": !at.After(w.UpdateTime), "sameInNested": at.Equal(nested)}, nil
	})
	update := func(ref *firestore.DocumentRef, initial obj, updates upd) func() (any, error) {
		return func() (any, error) {
			if _, err := ref.Set(ctx, initial); err != nil {
				return nil, err
			}
			if _, err := ref.Update(ctx, updates); err != nil {
				return nil, err
			}
			return data(ref)
		}
	}
	step("increment", update(users.Doc("counter"), obj{"n": 1, "f": 1.5}, upd{
		{Path: "n", Value: firestore.Increment(2)}, {Path: "f", Value: firestore.Increment(1)},
		{Path: "fresh", Value: firestore.Increment(5)}}))
	step("maximum and minimum", update(users.Doc("bounds"), obj{"a": 5, "b": 5, "c": 1.5}, upd{
		{Path: "a", Value: firestore.FieldTransformMaximum(7)}, {Path: "b", Value: firestore.FieldTransformMinimum(3)},
		{Path: "c", Value: firestore.FieldTransformMaximum(1)}, {Path: "fresh", Value: firestore.FieldTransformMinimum(2)}}))
	step("arrayUnion and arrayRemove", func() (any, error) {
		ref := users.Doc("tags")
		if _, err := ref.Set(ctx, obj{"tags": []any{"a", 1}}); err != nil {
			return nil, err
		}
		if _, err := ref.Update(ctx, upd{{Path: "tags", Value: firestore.ArrayUnion("b", "a", 1.0)}}); err != nil {
			return nil, err
		}
		afterUnion, err := data(ref)
		if err != nil {
			return nil, err
		}
		if _, err := ref.Update(ctx, upd{{Path: "tags", Value: firestore.ArrayRemove("a", 1)}}); err != nil {
			return nil, err
		}
		afterRemove, err := data(ref)
		return obj{"afterUnion": afterUnion.(obj)["tags"], "afterRemove": afterRemove.(obj)["tags"]}, err
	})
	step("delete a field", update(users.Doc("del"), obj{"keep": 1, "drop": 2, "m": obj{"x": 1, "y": 2}}, upd{
		{Path: "drop", Value: firestore.Delete}, {Path: "m.x", Value: firestore.Delete}}))
	step("merge with transforms", func() (any, error) {
		ref := users.Doc("merge")
		if _, err := ref.Set(ctx, obj{"a": 1, "list": []any{"x"}}); err != nil {
			return nil, err
		}
		if _, err := ref.Set(ctx, obj{"b": firestore.Increment(3), "list": firestore.ArrayUnion("y")}, firestore.MergeAll); err != nil {
			return nil, err
		}
		return data(ref)
	})
	step("update quoted field paths", update(users.Doc("paths"), obj{"a.b": obj{"c": 1}, "plain": 1}, upd{
		{FieldPath: firestore.FieldPath{"a.b", "c"}, Value: 2}, {FieldPath: firestore.FieldPath{"x y"}, Value: 3}}))
	step("value types", func() (any, error) {
		ref := users.Doc("types")
		_, err := ref.Set(ctx, obj{
			"null": nil, "true": true, "int": int64(math.MaxInt64), "negative": -42,
			"double": 0.1, "nan": math.NaN(), "inf": math.Inf(1), "-inf": math.Inf(-1),
			"string": "h\u00e9llo \u2603 \U0001f600", "bytes": []byte{0, 1, 2, 255},
			"timestamp": time.Date(2024, 1, 2, 3, 4, 5, 123456000, time.UTC),
			"geopoint":  &latlng.LatLng{Latitude: 35.68, Longitude: 139.76}, "reference": client.Doc("users/bob"),
			"array": []any{1, "a", nil, obj{"m": true}}, "map": obj{"a": obj{"b": obj{"c": 1}}},
			"emptyMap": obj{}, "emptyArray": []any{}, "vector": firestore.Vector64{1.0, 2.5},
		})
		if err != nil {
			return nil, err
		}
		return data(ref)
	})
}

// --- queries --------------------------------------------------------------------------------

func queries() {
	cities := client.Collection("cities")
	step("seed", func() (any, error) {
		b := client.Batch()
		for id, row := range map[string]obj{
			"SF": {"name": "San Francisco", "state": "CA", "country": "USA", "capital": false, "population": 860000,
				"regions": []any{"west_coast", "norcal"}, "loc": obj{"lat": 37.7}},
			"LA": {"name": "Los Angeles", "state": "CA", "country": "USA", "capital": false, "population": 3900000,
				"regions": []any{"west_coast", "socal"}, "loc": obj{"lat": 34.0}},
			"DC": {"name": "Washington, D.C.", "state": nil, "country": "USA", "capital": true, "population": 680000,
				"regions": []any{"east_coast"}},
			"TOK": {"name": "Tokyo", "state": nil, "country": "Japan", "capital": true, "population": 9000000,
				"regions": []any{"kanto", "honshu"}, "loc": obj{"lat": 35.7}},
			"BJ": {"name": "Beijing", "state": nil, "country": "China", "capital": true, "population": 21500000,
				"regions": []any{"jingjinji", "hebei"}},
			"NAN": {"name": "Nowhere", "country": "None", "population": math.NaN()},
		} {
			b.Set(cities.Doc(id), row)
		}
		b.Set(client.Doc("cities/SF/landmarks/golden-gate"), obj{"name": "Golden Gate Bridge", "type": "bridge"})
		b.Set(client.Doc("cities/SF/landmarks/legion"), obj{"name": "Legion of Honor", "type": "museum"})
		b.Set(client.Doc("cities/TOK/landmarks/national"), obj{"name": "National Museum", "type": "museum"})
		b.Set(client.Doc("cities/BJ/landmarks/forbidden"), obj{"name": "Forbidden City", "type": "museum"})
		for id, row := range map[string]obj{
			"a": {"total": 10, "qty": 1, "status": "paid"}, "b": {"total": 2.5, "qty": 3, "status": "paid"},
			"c": {"total": 7, "qty": 2, "status": "open"}, "d": {"total": "n/a", "qty": 1, "status": "open"},
			"e": {"qty": 5, "status": "void"},
		} {
			b.Set(client.Collection("orders").Doc(id), row)
		}
		b.Set(client.Doc("orders/a/lines/1"), obj{"qty": 4})
		b.Set(client.Doc("orders/b/lines/1"), obj{"qty": 6})
		b.Set(client.Doc("big/x"), obj{"n": 9007199254740991})
		b.Set(client.Doc("big/y"), obj{"n": 9007199254740991})
		for id, v := range map[string]firestore.Vector64{"a": {1, 0}, "b": {0, 1}, "c": {1, 1}, "d": {3, 4}} {
			b.Set(client.Collection("vectors").Doc(id), obj{"v": v, "k": id})
		}
		_, err := b.Commit(ctx)
		return "ok", err
	})
	where := func(path, op string, v any) func() (any, error) { return get(cities.Where(path, op, v)) }
	prop := func(path, op string, v any) firestore.PropertyFilter {
		return firestore.PropertyFilter{Path: path, Operator: op, Value: v}
	}
	step("all", get(cities))
	step("where ==", where("state", "==", "CA"))
	step("where == null (IS_NULL)", where("state", "==", nil))
	step("where != null (IS_NOT_NULL)", where("state", "!=", nil))
	step("where == NaN (IS_NAN)", where("population", "==", math.NaN()))
	step("where <", where("population", "<", 1000000))
	step("where >= orderBy desc", get(cities.Where("population", ">=", 1000000).OrderBy("population", firestore.Desc)))
	step("where !=", where("country", "!=", "USA"))
	step("where in", where("country", "in", []any{"Japan", "China"}))
	step("where not-in", where("country", "not-in", []any{"USA", "Japan"}))
	step("array-contains", where("regions", "array-contains", "west_coast"))
	step("array-contains-any", where("regions", "array-contains-any", []any{"west_coast", "kanto"}))
	step("nested field", where("loc.lat", ">", 35))
	step("two inequalities", get(cities.Where("population", ">", 700000).Where("name", "<", "U")))
	step("or", get(cities.WhereEntity(firestore.OrFilter{Filters: []firestore.EntityFilter{
		prop("capital", "==", true), prop("state", "==", "CA")}})))
	step("or of and", get(cities.WhereEntity(firestore.OrFilter{Filters: []firestore.EntityFilter{
		firestore.AndFilter{Filters: []firestore.EntityFilter{prop("state", "==", "CA"), prop("population", ">", 1000000)}},
		prop("country", "==", "Japan")}})))
	step("orderBy limit", get(cities.OrderBy("name", firestore.Asc).Limit(3)))
	step("limitToLast", get(cities.OrderBy("name", firestore.Asc).LimitToLast(2)))
	step("offset", get(cities.OrderBy("population", firestore.Asc).Offset(2).Limit(2)))
	step("startAt / endBefore values", get(cities.OrderBy("population", firestore.Asc).StartAt(860000).EndBefore(9000000)))
	step("startAfter a snapshot", func() (any, error) {
		sf, err := cities.Doc("SF").Get(ctx)
		if err != nil {
			return nil, err
		}
		return get(cities.OrderBy("country", firestore.Asc).StartAfter(sf))()
	})
	step("paging with startAfter", func() (any, error) {
		pages := []any{}
		q := cities.OrderBy("population", firestore.Asc).Limit(2)
		for {
			snaps, err := q.Documents(ctx).GetAll()
			if err != nil || len(snaps) == 0 {
				return pages, err
			}
			pages = append(pages, ids(snaps))
			q = cities.OrderBy("population", firestore.Asc).StartAfter(snaps[len(snaps)-1]).Limit(2)
		}
	})
	step("select", func() (any, error) {
		snaps, err := cities.Where("capital", "==", true).Select("name", "loc.lat").Documents(ctx).GetAll()
		l := []any{}
		for _, s := range snaps {
			l = append(l, obj{"id": s.Ref.ID, "data": norm(s.Data(), true)})
		}
		return l, err
	})
	step("documentId in", where(firestore.DocumentID, "in", []any{cities.Doc("TOK"), cities.Doc("SF")}))
	step("documentId range", get(cities.Where(firestore.DocumentID, ">=", cities.Doc("L")).Where(firestore.DocumentID, "<", cities.Doc("T"))))
	step("orderBy documentId desc", get(cities.OrderBy(firestore.DocumentID, firestore.Desc)))
	landmarks := client.CollectionGroup("landmarks")
	step("collectionGroup", get(landmarks.Where("type", "==", "museum")))
	step("collectionGroup orderBy name", get(landmarks.OrderBy("name", firestore.Asc)))
	step("subcollection", get(cities.Doc("SF").Collection("landmarks")))
	step("stream", func() (any, error) {
		it := cities.Where("capital", "==", true).Documents(ctx)
		defer it.Stop()
		l := []string{}
		for {
			s, err := it.Next()
			if err == iterator.Done {
				return l, nil
			}
			if err != nil {
				return nil, err
			}
			l = append(l, s.Ref.ID)
		}
	})
	in31 := []any{}
	for i := 0; i < 31; i++ {
		in31 = append(in31, i)
	}
	step("in with 31 values", where("population", "in", in31))
	step("explain", func() (any, error) {
		it := cities.Where("capital", "==", true).WithRunOptions(firestore.ExplainOptions{Analyze: true}).Documents(ctx)
		var snaps []*firestore.DocumentSnapshot
		for {
			s, err := it.Next()
			if err == iterator.Done {
				break
			}
			if err != nil {
				return nil, err
			}
			snaps = append(snaps, s)
		}
		var metrics any = "present"
		if m, err := it.ExplainMetrics(); err != nil {
			metrics = errorOf(err)
		} else if m == nil {
			metrics = nil
		}
		return obj{"docs": ids(snaps), "metrics": metrics}, nil
	})
	step("query in a transaction", func() (any, error) {
		var result any
		err := client.RunTransaction(ctx, func(ctx context.Context, tx *firestore.Transaction) error {
			snaps, err := tx.Documents(cities.Where("capital", "==", true)).GetAll()
			if err != nil {
				return err
			}
			result = ids(snaps)
			return tx.Update(cities.Doc("DC"), upd{{Path: "visited", Value: true}})
		})
		return result, err
	})
	step("read-only transaction query", func() (any, error) {
		var result any
		err := client.RunTransaction(ctx, func(ctx context.Context, tx *firestore.Transaction) error {
			snaps, err := tx.Documents(cities.Where("state", "==", "CA")).GetAll()
			result = ids(snaps)
			return err
		}, firestore.ReadOnly)
		return result, err
	})
	step("partition query", func() (any, error) {
		qs, err := landmarks.GetPartitionedQueries(ctx, 2)
		return len(qs), err
	})

	orders := client.Collection("orders")
	agg := func(a *firestore.AggregationQuery) func() (any, error) {
		return func() (any, error) {
			r, err := a.Get(ctx)
			if err != nil {
				return nil, err
			}
			return norm(r.Data(), true), nil
		}
	}
	paid := orders.Where("status", "==", "paid")
	open := orders.Where("status", "==", "open")
	none := orders.Where("status", "==", "none")
	limited := orders.Limit(2)
	big := client.Collection("big")
	lines := client.CollectionGroup("lines")
	step("count", agg(orders.NewAggregationQuery().WithCount("count")))
	step("count with a filter", agg(open.NewAggregationQuery().WithCount("count")))
	step("count with a limit", agg(limited.NewAggregationQuery().WithCount("count")))
	step("sum and average", agg(orders.NewAggregationQuery().WithSum("total", "total").WithAvg("total", "mean")))
	step("count next to sum", agg(orders.NewAggregationQuery().WithCount("n").WithSum("qty", "qty")))
	step("average of nothing", agg(none.NewAggregationQuery().WithAvg("total", "mean").WithSum("total", "total")))
	step("sum beyond 2^53", agg(big.NewAggregationQuery().WithSum("n", "n")))
	step("collection group count and sum", agg(lines.NewAggregationQuery().WithCount("n").WithSum("qty", "qty")))
	step("count in a transaction", func() (any, error) {
		var result any
		err := client.RunTransaction(ctx, func(ctx context.Context, tx *firestore.Transaction) error {
			r, err := paid.NewAggregationQuery().WithCount("count").Transaction(tx).Get(ctx)
			if err != nil {
				return err
			}
			result = norm(r.Data(), true)
			return tx.Set(orders.Doc("f"), obj{"total": 1, "qty": 1, "status": "paid"})
		})
		return result, err
	})
	step("count after the transaction", agg(paid.NewAggregationQuery().WithCount("count")))
	nearest := func(measure firestore.DistanceMeasure) func() (any, error) {
		return func() (any, error) {
			snaps, err := client.Collection("vectors").FindNearest("v", firestore.Vector64{1.0, 0.5}, 3, measure,
				&firestore.FindNearestOptions{DistanceResultField: "distance"}).Documents(ctx).GetAll()
			l := []any{}
			for _, s := range snaps {
				l = append(l, []any{s.Ref.ID, norm(s.Data()["distance"], true)})
			}
			return l, err
		}
	}
	step("findNearest euclidean", nearest(firestore.DistanceMeasureEuclidean))
	step("findNearest dot product", nearest(firestore.DistanceMeasureDotProduct))
	// The Go SDK has no recursiveDelete.
}

// --- transactions ---------------------------------------------------------------------------

// attempts runs fn in a transaction and counts how many attempts it took.
func attempts(fn func(tx *firestore.Transaction) (any, error), opts ...firestore.TransactionOption) (obj, error) {
	n := 0
	var result any
	err := client.RunTransaction(ctx, func(_ context.Context, tx *firestore.Transaction) error {
		n++
		var err error
		result, err = fn(tx)
		return err
	}, opts...)
	if err != nil {
		return nil, err
	}
	return obj{"attempts": n, "result": result}, nil
}

// txGet reads a document in a transaction; a missing one is a snapshot that does not exist.
func txGet(tx *firestore.Transaction, ref *firestore.DocumentRef) (*firestore.DocumentSnapshot, error) {
	s, err := tx.Get(ref)
	if status.Code(err) == codes.NotFound {
		return s, nil
	}
	return s, err
}

func with(o obj, err error, key string, ref *firestore.DocumentRef) (any, error) {
	if err != nil {
		return nil, err
	}
	d, err := data(ref)
	o[key] = d
	return o, err
}

func transactions() {
	c := client.Collection("txn")
	a := c.Doc("a")
	step("create when missing", func() (any, error) {
		o, err := attempts(func(tx *firestore.Transaction) (any, error) {
			s, err := txGet(tx, a)
			if err != nil {
				return nil, err
			}
			if !s.Exists() {
				return false, tx.Set(a, obj{"n": 0})
			}
			return true, nil
		})
		return with(o, err, "data", a)
	})
	step("read-modify-write", func() (any, error) {
		o, err := attempts(func(tx *firestore.Transaction) (any, error) {
			s, err := txGet(tx, a)
			if err != nil {
				return nil, err
			}
			n := s.Data()["n"].(int64)
			return n, tx.Update(a, upd{{Path: "n", Value: n + 1}})
		})
		return with(o, err, "data", a)
	})
	step("getAll", func() (any, error) {
		return attempts(func(tx *firestore.Transaction) (any, error) {
			snaps, err := tx.GetAll([]*firestore.DocumentRef{a, c.Doc("missing")})
			l := []any{}
			for _, s := range snaps {
				l = append(l, obj{"id": s.Ref.ID, "exists": s.Exists()})
			}
			return l, err
		})
	})
	step("write-only transaction", func() (any, error) {
		o, err := attempts(func(tx *firestore.Transaction) (any, error) {
			if err := tx.Set(c.Doc("b"), obj{"n": 1}); err != nil {
				return nil, err
			}
			return nil, tx.Delete(c.Doc("nothing"))
		})
		return with(o, err, "data", c.Doc("b"))
	})
	readA := func(tx *firestore.Transaction) (any, error) {
		s, err := txGet(tx, a)
		if err != nil {
			return nil, err
		}
		return norm(s.Data(), true), nil
	}
	var snapshotTime time.Time
	step("read-only", func() (any, error) {
		s, err := a.Get(ctx)
		if err != nil {
			return nil, err
		}
		snapshotTime = s.ReadTime
		return attempts(readA, firestore.ReadOnly)
	})
	step("read-only at a past read time", func() (any, error) {
		if _, err := a.Update(ctx, upd{{Path: "n", Value: 100}}); err != nil {
			return nil, err
		}
		return attempts(readA, firestore.TransactionReadTime(snapshotTime))
	})
	step("read-only cannot write", func() (any, error) {
		return attempts(func(tx *firestore.Transaction) (any, error) {
			return nil, tx.Set(c.Doc("ro"), obj{"n": 1})
		}, firestore.ReadOnly)
	})
	step("a failing callback rolls back and releases its locks", func() (any, error) {
		_, err := attempts(func(tx *firestore.Transaction) (any, error) {
			if _, err := txGet(tx, a); err != nil {
				return nil, err
			}
			return nil, errors.New("boom")
		})
		start := time.Now()
		if _, err := a.Set(ctx, obj{"n": 1}); err != nil {
			return nil, err
		}
		return obj{"failed": err.Error(), "outsideWriteWaited": time.Since(start) > time.Second}, nil
	})
	step("create of an existing document", func() (any, error) {
		return attempts(func(tx *firestore.Transaction) (any, error) {
			if _, err := txGet(tx, c.Doc("b")); err != nil {
				return nil, err
			}
			return nil, tx.Create(c.Doc("b"), obj{"n": 2})
		})
	})
	// holding runs a transaction that reads ref, holds it and writes it, in a goroutine; it
	// returns once the transaction has read, with a function that waits for its outcome.
	holding := func(ref *firestore.DocumentRef, hold time.Duration) func() any {
		reading := make(chan struct{})
		var once sync.Once
		done := make(chan any)
		go func() {
			o, err := attempts(func(tx *firestore.Transaction) (any, error) {
				if _, err := txGet(tx, ref); err != nil {
					return nil, err
				}
				once.Do(func() { close(reading) })
				time.Sleep(hold)
				return nil, tx.Set(ref, obj{"by": "transaction"})
			})
			if err != nil {
				done <- errorOf(err)
			} else {
				done <- o
			}
		}()
		<-reading
		return func() any { return <-done }
	}
	outsideWrite := func(ref *firestore.DocumentRef) (any, time.Duration) {
		start := time.Now()
		if _, err := ref.Set(ctx, obj{"by": "outside"}); err != nil {
			return errorOf(err), time.Since(start)
		}
		return "ok", time.Since(start)
	}
	step("an outside write waits for the transaction that read the document", func() (any, error) {
		ref := c.Doc("w")
		wait := holding(ref, 500*time.Millisecond)
		result, waited := outsideWrite(ref)
		o := obj{"outside": result, "outsideWaited": waited > 300*time.Millisecond, "transaction": wait()}
		return with(o, nil, "final", ref)
	})
	step("an outside write gives up after the lock timeout", func() (any, error) {
		ref := c.Doc("t")
		wait := holding(ref, 3*time.Second)
		result, _ := outsideWrite(ref)
		o := obj{"outside": result, "transaction": wait()}
		return with(o, nil, "final", ref)
	})
	step("two transactions increment the same counter", func() (any, error) {
		ref := c.Doc("counter")
		if _, err := ref.Set(ctx, obj{"n": 0}); err != nil {
			return nil, err
		}
		var mu sync.Mutex
		var wg sync.WaitGroup
		total := 0
		var failure error
		// Both read the counter, then commit 300 ms apart: each commit waits for the other's
		// lock, the first wait times out after 2 s and ends the deadlock, one transaction is
		// retried. (Commits at the same instant can both time out, which makes the number of
		// attempts depend on the SDK's backoff.)
		for _, delay := range []time.Duration{100 * time.Millisecond, 400 * time.Millisecond} {
			wg.Add(1)
			go func() {
				defer wg.Done()
				o, err := attempts(func(tx *firestore.Transaction) (any, error) {
					s, err := txGet(tx, ref)
					if err != nil {
						return nil, err
					}
					time.Sleep(delay)
					return nil, tx.Update(ref, upd{{Path: "n", Value: s.Data()["n"].(int64) + 1}})
				})
				mu.Lock()
				defer mu.Unlock()
				if err != nil {
					failure = err
				} else {
					total += o["attempts"].(int)
				}
			}()
		}
		wg.Wait()
		if failure != nil {
			return nil, failure
		}
		return with(obj{"totalAttempts": total}, nil, "final", ref)
	})
}

// --- listen ---------------------------------------------------------------------------------

type listenStep struct {
	Step      string           `json:"step"`
	Listeners map[string][]any `json:"listeners"`
}

func listen() {
	c := client.Collection("cities")
	var mu sync.Mutex
	var steps []*listenStep
	last := time.Now()
	record := func(listener string, entry any) {
		mu.Lock()
		defer mu.Unlock()
		s := steps[len(steps)-1]
		s.Listeners[listener] = append(s.Listeners[listener], entry)
		last = time.Now()
	}
	quiet := func() {
		time.Sleep(150 * time.Millisecond)
		for {
			mu.Lock()
			idle := time.Since(last)
			mu.Unlock()
			if idle >= 300*time.Millisecond {
				return
			}
			time.Sleep(50 * time.Millisecond)
		}
	}
	section := func(name string, fn func() error) {
		mu.Lock()
		steps = append(steps, &listenStep{Step: name, Listeners: map[string][]any{}})
		mu.Unlock()
		if err := fn(); err != nil {
			record("step", errorOf(err))
		}
		quiet()
	}
	var stops []context.CancelFunc
	var wg sync.WaitGroup
	kinds := map[firestore.DocumentChangeKind]string{
		firestore.DocumentAdded: "added", firestore.DocumentModified: "modified", firestore.DocumentRemoved: "removed",
	}
	// Each listener stops when its context is cancelled (Stop may not be called concurrently
	// with Next).
	queryListener := func(name string, q firestore.Query) {
		lctx, cancel := context.WithCancel(ctx)
		it := q.Snapshots(lctx)
		stops = append(stops, cancel)
		wg.Add(1)
		go func() {
			defer wg.Done()
			defer it.Stop()
			for {
				snap, err := it.Next()
				if err != nil {
					if err != iterator.Done && status.Code(err) != codes.Canceled {
						record(name, obj{"error": errorOf(err)})
					}
					return
				}
				docs, err := snap.Documents.GetAll()
				if err != nil {
					record(name, obj{"error": errorOf(err)})
					return
				}
				ds := []any{}
				for _, d := range docs {
					ds = append(ds, []any{d.Ref.ID, norm(d.Data(), false)})
				}
				changes := []any{}
				for _, ch := range snap.Changes {
					changes = append(changes, []any{kinds[ch.Kind], ch.Doc.Ref.ID, ch.OldIndex, ch.NewIndex})
				}
				record(name, obj{"docs": ds, "changes": changes})
			}
		}()
	}
	docListener := func(name string, ref *firestore.DocumentRef) {
		lctx, cancel := context.WithCancel(ctx)
		it := ref.Snapshots(lctx)
		stops = append(stops, cancel)
		wg.Add(1)
		go func() {
			defer wg.Done()
			defer it.Stop()
			for {
				snap, err := it.Next()
				if err != nil {
					if err != iterator.Done && status.Code(err) != codes.Canceled {
						record(name, obj{"error": errorOf(err)})
					}
					return
				}
				var d any
				if snap.Exists() {
					d = norm(snap.Data(), false)
				}
				record(name, obj{"exists": snap.Exists(), "data": d})
			}
		}()
	}
	set := func(ref *firestore.DocumentRef, v obj) func() error {
		return func() error { _, err := ref.Set(ctx, v); return err }
	}
	update := func(ref *firestore.DocumentRef, u upd) func() error {
		return func() error { _, err := ref.Update(ctx, u); return err }
	}
	section("seed", func() error {
		b := client.Batch()
		b.Set(c.Doc("SF"), obj{"pop": 860, "state": "CA"})
		b.Set(c.Doc("LA"), obj{"pop": 3900, "state": "CA"})
		b.Set(c.Doc("TOK"), obj{"pop": 9000, "state": nil})
		_, err := b.Commit(ctx)
		return err
	})
	section("attach listeners", func() error {
		queryListener("all", c.Query)
		queryListener("pop > 1000", c.Where("pop", ">", 1000))
		queryListener("CA by pop", c.Where("state", "==", "CA").OrderBy("pop", firestore.Asc))
		queryListener("top 2 by pop", c.OrderBy("pop", firestore.Desc).Limit(2))
		docListener("doc SF", c.Doc("SF"))
		docListener("doc NY", c.Doc("NY"))
		queryListener("group landmarks", client.CollectionGroup("landmarks").Query)
		return nil
	})
	section("add NY (enters several queries)", set(c.Doc("NY"), obj{"pop": 8300, "state": "NY"}))
	section("update SF (stays in all)", update(c.Doc("SF"), upd{{Path: "pop", Value: 870}}))
	section("update LA below 1000 (leaves pop > 1000, moves in order)", update(c.Doc("LA"), upd{{Path: "pop", Value: 999}}))
	section("update TOK above all (changes the top 2)", update(c.Doc("TOK"), upd{{Path: "pop", Value: 9500}}))
	section("no-op write", set(c.Doc("SF"), obj{"pop": 870, "state": "CA"}))
	section("write to another collection", set(client.Doc("other/x"), obj{"n": 1}))
	section("batch: delete TOK, add BJ, change NY state", func() error {
		b := client.Batch()
		b.Delete(c.Doc("TOK"))
		b.Set(c.Doc("BJ"), obj{"pop": 21500, "state": nil})
		b.Update(c.Doc("NY"), upd{{Path: "state", Value: "CA"}})
		_, err := b.Commit(ctx)
		return err
	})
	section("add a landmark", set(client.Doc("cities/SF/landmarks/gg"), obj{"name": "Golden Gate"}))
	section("server timestamp", update(c.Doc("SF"), upd{{Path: "at", Value: firestore.ServerTimestamp}}))
	section("delete NY", func() error { _, err := c.Doc("NY").Delete(ctx); return err })
	section("detach", func() error {
		for _, stop := range stops {
			stop()
		}
		wg.Wait()
		return nil
	})
	// Listeners fire in no particular order: within each step the snapshots are grouped by
	// listener (encoding/json sorts the names), each listener's own order kept.
	for _, s := range steps {
		out = append(out, s)
	}
}
