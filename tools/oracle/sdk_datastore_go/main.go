// Command sdk_datastore_go runs one section of steps through cloud.google.com/go/datastore (the
// Go client library of Datastore) and prints a transcript, so the transcripts of two emulators in
// Datastore mode can be diffed (#81). DATASTORE_EMULATOR_HOST selects the emulator, started with
// `--database-mode datastore-mode`; start it fresh for each section.
//
// Usage: go run . entities|queries|transactions > transcript.json
//
// The sections and their step names are those of tools/oracle/sdk_datastore_python/;
// tools/oracle/README.md says how to run them.
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
	"strconv"
	"strings"
	"time"

	"cloud.google.com/go/datastore"
	"cloud.google.com/go/datastore/apiv1/datastorepb"
	"google.golang.org/api/iterator"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

var (
	ctx     = context.Background()
	project string
	client  *datastore.Client
	out     []any
)

type obj = map[string]any

func main() {
	sections := map[string]func(){"entities": entities, "queries": queries, "transactions": transactions}
	if len(os.Args) != 2 || sections[os.Args[1]] == nil {
		fmt.Fprintln(os.Stderr, "usage: go run . entities|queries|transactions > transcript.json")
		os.Exit(2)
	}
	project = "demo-datastore-go-" + os.Args[1]
	var err error
	client, err = datastore.NewClient(ctx, project)
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

func mask(s string) string {
	return digits.ReplaceAllString(strings.ReplaceAll(s, project, "{project}"), "<n>")
}

// errorOf is a server error as its gRPC code and message; anything else as a client error.
func errorOf(err error) obj {
	var grpcErr interface{ GRPCStatus() *status.Status }
	if errors.As(err, &grpcErr) {
		s := grpcErr.GRPCStatus()
		return obj{"code": codeNames[s.Code()], "message": mask(s.Message())}
	}
	return obj{"client": mask(err.Error())}
}

func step(name string, fn func() (any, error)) {
	v, err := fn()
	if err != nil {
		out = append(out, obj{"name": name, "error": errorOf(err)})
		return
	}
	out = append(out, obj{"name": name, "ok": v})
}

func number(f float64) any {
	switch {
	case math.IsNaN(f):
		return "NaN"
	case math.IsInf(f, 1):
		return "Infinity"
	case math.IsInf(f, -1):
		return "-Infinity"
	}
	s := strconv.FormatFloat(f, 'f', -1, 64)
	if !strings.Contains(s, ".") {
		s += ".0"
	}
	return json.Number(s)
}

func keyOf(k *datastore.Key) any {
	var path []any
	for ; k != nil; k = k.Parent {
		var id any = k.Name
		if k.Name == "" {
			id = k.ID
		}
		path = append([]any{k.Kind, id}, path...)
		if k.Parent == nil && k.Namespace != "" {
			return obj{"path": path, "namespace": k.Namespace}
		}
	}
	return obj{"path": path}
}

func norm(v any) any {
	switch v := v.(type) {
	case nil, bool, string, int64:
		return v
	case float64:
		return number(v)
	case time.Time:
		return v.UTC().Format(time.RFC3339Nano)
	case []byte:
		return obj{"bytes": base64.StdEncoding.EncodeToString(v)}
	case datastore.GeoPoint:
		return obj{"geopoint": []any{number(v.Lat), number(v.Lng)}}
	case *datastore.Key:
		return obj{"key": keyOf(v)}
	case *datastore.Entity:
		return entity(v.Key, v.Properties)
	case []any:
		items := []any{}
		for _, x := range v {
			items = append(items, norm(x))
		}
		return items
	}
	return obj{"unknown": fmt.Sprint(v)}
}

func entity(key *datastore.Key, props []datastore.Property) any {
	p := obj{}
	for _, prop := range props {
		p[prop.Name] = norm(prop.Value)
	}
	e := obj{"properties": p}
	if key != nil {
		e["key"] = keyOf(key)
	}
	return e
}

func get(k *datastore.Key) (any, error) {
	var pl datastore.PropertyList
	err := client.Get(ctx, k, &pl)
	if errors.Is(err, datastore.ErrNoSuchEntity) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	return entity(k, pl), nil
}

func props(kv ...any) *datastore.PropertyList {
	pl := datastore.PropertyList{}
	for i := 0; i < len(kv); i += 2 {
		pl = append(pl, datastore.Property{Name: kv[i].(string), Value: kv[i+1]})
	}
	return &pl
}

func keys(ks []*datastore.Key) any {
	out := []any{}
	for _, k := range ks {
		out = append(out, keyOf(k))
	}
	return out
}

// --- entities -------------------------------------------------------------------------------

func entities() {
	task := datastore.NameKey("Task", "t1", nil)
	every := datastore.PropertyList{
		{Name: "text", Value: "a"}, {Name: "count", Value: int64(3)}, {Name: "ratio", Value: 0.5},
		{Name: "done", Value: false}, {Name: "nothing", Value: nil},
		{Name: "at", Value: time.Date(2020, 1, 2, 3, 4, 5, 678000000, time.UTC)},
		{Name: "raw", Value: []byte{0, 1}}, {Name: "place", Value: datastore.GeoPoint{Lat: 35, Lng: 139}},
		{Name: "owner", Value: datastore.NameKey("User", "u1", nil)},
		{Name: "tags", Value: []any{"x", "y"}}, {Name: "empty", Value: []any{}},
		{Name: "nested", Value: &datastore.Entity{Properties: []datastore.Property{
			{Name: "x", Value: int64(1)}, {Name: "y", Value: "z"}}}},
		{Name: "long", Value: strings.Repeat("b", 2000), NoIndex: true},
		{Name: "big", Value: int64(1) << 62}, {Name: "nan", Value: math.NaN()},
		{Name: "inf", Value: math.Inf(1)}, {Name: "ninf", Value: math.Inf(-1)},
		{Name: "zero", Value: math.Copysign(0, -1)},
	}
	step("put every type", func() (any, error) {
		_, err := client.Put(ctx, task, &every)
		return "ok", err
	})
	step("get it back", func() (any, error) { return get(task) })
	step("get a missing entity", func() (any, error) { return get(datastore.NameKey("Task", "nobody", nil)) })
	step("get several, one missing", func() (any, error) {
		dst := make([]datastore.PropertyList, 2)
		err := client.GetMulti(ctx, []*datastore.Key{task, datastore.NameKey("Task", "nobody", nil)}, dst)
		var multi datastore.MultiError
		if errors.As(err, &multi) {
			found := []any{}
			for _, e := range multi {
				found = append(found, e == nil)
			}
			return found, nil
		}
		return nil, err
	})
	step("overwrite it", func() (any, error) {
		if _, err := client.Put(ctx, task, props("text", "b")); err != nil {
			return nil, err
		}
		return get(task)
	})
	step("put an incomplete key", func() (any, error) {
		k, err := client.Put(ctx, datastore.IncompleteKey("Task", nil), props("text", "generated"))
		if err != nil {
			return nil, err
		}
		read, err := get(k)
		return obj{"id": k.ID, "read": read}, err
	})
	step("allocate three IDs", func() (any, error) {
		inc := datastore.IncompleteKey("Task", nil)
		ks, err := client.AllocateIDs(ctx, []*datastore.Key{inc, inc, inc})
		ids := []any{}
		for _, k := range ks {
			ids = append(ids, k.ID)
		}
		return ids, err
	})
	step("allocate an ID under a parent", func() (any, error) {
		ks, err := client.AllocateIDs(ctx, []*datastore.Key{datastore.IncompleteKey("Task", datastore.NameKey("List", "l1", nil))})
		return keys(ks), err
	})
	step("reserve an ID", func() (any, error) {
		return "ok", client.ReserveIDs(ctx, []*datastore.Key{datastore.IDKey("Task", 5, nil)})
	})
	child := datastore.NameKey("Task", "c1", datastore.NameKey("List", "l1", nil))
	step("put a child", func() (any, error) {
		if _, err := client.Put(ctx, child, props("text", "child")); err != nil {
			return nil, err
		}
		return get(child)
	})
	step("get a child's parent, never written", func() (any, error) { return get(child.Parent) })
	step("put in a namespace", func() (any, error) {
		k := datastore.NameKey("Task", "t1", nil)
		k.Namespace = "ns1"
		if _, err := client.Put(ctx, k, props("text", "ns")); err != nil {
			return nil, err
		}
		return get(k)
	})
	step("put several", func() (any, error) {
		ks := []*datastore.Key{datastore.IDKey("Many", 1, nil), datastore.IDKey("Many", 2, nil), datastore.IDKey("Many", 3, nil)}
		src := []datastore.PropertyList{*props("i", int64(1)), *props("i", int64(2)), *props("i", int64(3))}
		_, err := client.PutMulti(ctx, ks, src)
		return "ok", err
	})
	step("delete several", func() (any, error) {
		err := client.DeleteMulti(ctx, []*datastore.Key{datastore.IDKey("Many", 1, nil), datastore.IDKey("Many", 2, nil)})
		if err != nil {
			return nil, err
		}
		return get(datastore.IDKey("Many", 3, nil))
	})
	step("delete a missing entity", func() (any, error) {
		return "ok", client.Delete(ctx, datastore.NameKey("Task", "nobody", nil))
	})
	step("delete it", func() (any, error) {
		if err := client.Delete(ctx, task); err != nil {
			return nil, err
		}
		return get(task)
	})
	step("insert an existing entity", func() (any, error) {
		k := datastore.NameKey("Task", "dup", nil)
		if _, err := client.Put(ctx, k, props("a", int64(1))); err != nil {
			return nil, err
		}
		_, err := client.Mutate(ctx, datastore.NewInsert(k, props("a", int64(2))))
		return "ok", err
	})
	step("update a missing entity", func() (any, error) {
		_, err := client.Mutate(ctx, datastore.NewUpdate(datastore.NameKey("Task", "nobody", nil), props("a", int64(1))))
		return "ok", err
	})
	step("an indexed string of 1501 bytes", func() (any, error) {
		_, err := client.Put(ctx, datastore.NameKey("Task", "long", nil), props("text", strings.Repeat("a", 1501)))
		return "ok", err
	})
	step("a property named __x__", func() (any, error) {
		_, err := client.Put(ctx, datastore.NameKey("Task", "r", nil), props("__x__", int64(1)))
		return "ok", err
	})
	step("an array in an array", func() (any, error) {
		_, err := client.Put(ctx, datastore.NameKey("Task", "r", nil), props("a", []any{[]any{int64(1)}}))
		return "ok", err
	})
	step("a kind __x__", func() (any, error) {
		_, err := client.Put(ctx, datastore.NameKey("__x__", "a", nil), props("a", int64(1)))
		return "ok", err
	})
}

// aggregated is an aggregation's result value: an integer, a double or null.
func aggregated(v any) any {
	value, ok := v.(*datastorepb.Value)
	if !ok {
		return obj{"unknown": fmt.Sprint(v)}
	}
	switch t := value.GetValueType().(type) {
	case *datastorepb.Value_IntegerValue:
		return t.IntegerValue
	case *datastorepb.Value_DoubleValue:
		return number(t.DoubleValue)
	case *datastorepb.Value_NullValue:
		return nil
	}
	return obj{"unknown": value.String()}
}

// --- queries --------------------------------------------------------------------------------

func run(q *datastore.Query) (any, error) {
	ks, err := client.GetAll(ctx, q.KeysOnly(), nil)
	return keys(ks), err
}

func projected(q *datastore.Query) (any, error) {
	var pls []datastore.PropertyList
	ks, err := client.GetAll(ctx, q, &pls)
	rows := []any{}
	for i, pl := range pls {
		rows = append(rows, entity(ks[i], pl))
	}
	return rows, err
}

func queries() {
	people := []struct {
		name string
		pl   *datastore.PropertyList
	}{
		{"p1", props("a", int64(1), "b", "x", "tags", []any{"red", "blue"})},
		{"p2", props("a", int64(2), "b", "y", "tags", []any{"red"})},
		{"p3", props("a", int64(2), "b", "x", "tags", []any{})},
		{"p4", props("a", int64(3), "b", "z", "tags", []any{"blue", "green"})},
		{"p5", props("a", int64(3), "b", "y")},
		{"p6", props("a", int64(-1), "b", "x", "tags", []any{"green"})},
		{"p7", props("b", "w")},
		{"p8", props("a", 4.5, "b", "x")},
	}
	for _, p := range people {
		if _, err := client.Put(ctx, datastore.NameKey("P", p.name, nil), p.pl); err != nil {
			panic(err)
		}
	}
	root := datastore.NameKey("F", "root", nil)
	for k, pl := range map[*datastore.Key]*datastore.PropertyList{
		root: props("n", int64(0)), datastore.NameKey("C", "c1", root): props("n", int64(1)),
		datastore.NameKey("C", "c2", root): props("n", int64(2)), datastore.NameKey("C", "c3", nil): props("n", int64(3)),
	} {
		if _, err := client.Put(ctx, k, pl); err != nil {
			panic(err)
		}
	}
	ns := datastore.NameKey("P", "n1", nil)
	ns.Namespace = "ns1"
	if _, err := client.Put(ctx, ns, props("a", int64(1), "b", "x")); err != nil {
		panic(err)
	}
	P := func() *datastore.Query { return datastore.NewQuery("P") }

	step("kind P", func() (any, error) { return run(P()) })
	for _, f := range []struct {
		op    string
		value any
	}{{"=", int64(2)}, {"<", int64(2)}, {"<=", int64(2)}, {">", int64(2)}, {">=", int64(2)},
		{"!=", int64(2)}, {"=", "x"}, {"in", []any{int64(1), int64(3)}}, {"not-in", []any{int64(2), int64(3)}},
		{"=", nil}, {"=", 4.5}, {"=", 2.0}} {
		f := f
		step(fmt.Sprintf("a %s %v", f.op, f.value), func() (any, error) { return run(P().FilterField("a", f.op, f.value)) })
	}
	step("tags = red", func() (any, error) { return run(P().FilterField("tags", "=", "red")) })
	step("a = 2 and b = x", func() (any, error) {
		return run(P().FilterEntity(datastore.AndFilter{Filters: []datastore.EntityFilter{
			datastore.PropertyFilter{FieldName: "a", Operator: "=", Value: int64(2)},
			datastore.PropertyFilter{FieldName: "b", Operator: "=", Value: "x"}}}))
	})
	step("a = 1 or b = y", func() (any, error) {
		return run(P().FilterEntity(datastore.OrFilter{Filters: []datastore.EntityFilter{
			datastore.PropertyFilter{FieldName: "a", Operator: "=", Value: int64(1)},
			datastore.PropertyFilter{FieldName: "b", Operator: "=", Value: "y"}}}))
	})
	step("a > 1 and b > w", func() (any, error) { return run(P().FilterField("a", ">", int64(1)).FilterField("b", ">", "w")) })
	step("order by a", func() (any, error) { return run(P().Order("a")) })
	step("order by -a, b", func() (any, error) { return run(P().Order("-a").Order("b")) })
	step("order by b, -__key__", func() (any, error) { return run(P().Order("b").Order("-__key__")) })
	step("__key__ > p4", func() (any, error) { return run(P().FilterField("__key__", ">", datastore.NameKey("P", "p4", nil))) })
	step("project a, b", func() (any, error) { return projected(P().Project("a", "b")) })
	step("distinct on b", func() (any, error) { return projected(P().Project("b").DistinctOn("b")) })
	step("limit 2", func() (any, error) { return run(P().Limit(2)) })
	step("offset 6", func() (any, error) { return run(P().Offset(6)) })
	step("limit 2, offset 3", func() (any, error) { return run(P().Limit(2).Offset(3)) })
	step("pages of 3 by cursor", func() (any, error) {
		pages := []any{}
		var cursor *datastore.Cursor
		for i := 0; i < 5; i++ {
			q := P().Order("b").Limit(3).KeysOnly()
			if cursor != nil {
				q = q.Start(*cursor)
			}
			it := client.Run(ctx, q)
			page := []*datastore.Key{}
			for {
				k, err := it.Next(nil)
				if err == iterator.Done {
					break
				}
				if err != nil {
					return nil, err
				}
				page = append(page, k)
			}
			if len(page) == 0 {
				break
			}
			pages = append(pages, keys(page))
			c, err := it.Cursor()
			if err != nil {
				return nil, err
			}
			cursor = &c
		}
		return pages, nil
	})
	step("children of F root", func() (any, error) { return run(datastore.NewQuery("C").Ancestor(root)) })
	step("everything under F root", func() (any, error) { return run(datastore.NewQuery("").Ancestor(root)) })
	step("kind P in ns1", func() (any, error) { return run(P().Namespace("ns1")) })
	step("kinds", func() (any, error) { return run(datastore.NewQuery("__kind__")) })
	step("namespaces", func() (any, error) { return run(datastore.NewQuery("__namespace__")) })
	step("a kind with no entities", func() (any, error) { return run(datastore.NewQuery("Nothing")) })
	aggregate := func(aq *datastore.AggregationQuery) (any, error) {
		res, err := client.RunAggregationQuery(ctx, aq)
		if err != nil {
			return nil, err
		}
		o := obj{}
		for alias, v := range res {
			o[alias] = aggregated(v)
		}
		return o, nil
	}
	step("count P", func() (any, error) { return aggregate(P().NewAggregationQuery().WithCount("n")) })
	step("count b = x", func() (any, error) {
		return aggregate(P().FilterField("b", "=", "x").NewAggregationQuery().WithCount("n"))
	})
	step("sum and avg of a", func() (any, error) {
		return aggregate(P().NewAggregationQuery().WithSum("a", "s").WithAvg("a", "m"))
	})
	step("IN with 31 values", func() (any, error) {
		values := []any{}
		for i := 0; i < 31; i++ {
			values = append(values, int64(i))
		}
		return run(P().FilterField("a", "in", values))
	})
	step("order twice by a", func() (any, error) { return run(P().Order("a").Order("-a")) })
	step("project a, filter a = 1", func() (any, error) { return projected(P().Project("a").FilterField("a", "=", int64(1))) })
}

// --- transactions ---------------------------------------------------------------------------

func transactions() {
	account := datastore.NameKey("Account", "a1", nil)
	if _, err := client.Put(ctx, account, props("balance", int64(100))); err != nil {
		panic(err)
	}
	balance := func() (any, error) {
		var pl datastore.PropertyList
		if err := client.Get(ctx, account, &pl); err != nil {
			return nil, err
		}
		return pl[0].Value, nil
	}
	step("read and write in a transaction", func() (any, error) {
		_, err := client.RunInTransaction(ctx, func(tx *datastore.Transaction) error {
			var pl datastore.PropertyList
			if err := tx.Get(account, &pl); err != nil {
				return err
			}
			_, err := tx.Put(account, props("balance", pl[0].Value.(int64)-30))
			return err
		})
		if err != nil {
			return nil, err
		}
		return balance()
	})
	step("roll back", func() (any, error) {
		tx, err := client.NewTransaction(ctx)
		if err != nil {
			return nil, err
		}
		if _, err := tx.Put(account, props("balance", int64(0))); err != nil {
			return nil, err
		}
		if err := tx.Rollback(); err != nil {
			return nil, err
		}
		return balance()
	})
	step("an error rolls back", func() (any, error) {
		_, err := client.RunInTransaction(ctx, func(tx *datastore.Transaction) error {
			if _, err := tx.Put(account, props("balance", int64(1))); err != nil {
				return err
			}
			return errors.New("stop")
		})
		return nil, err
	})
	step("after it", balance)
	step("read only", func() (any, error) {
		tx, err := client.NewTransaction(ctx, datastore.ReadOnly)
		if err != nil {
			return nil, err
		}
		var pl datastore.PropertyList
		if err := tx.Get(account, &pl); err != nil {
			return nil, err
		}
		return pl[0].Value, tx.Rollback()
	})
	step("a write in a read-only transaction", func() (any, error) {
		tx, err := client.NewTransaction(ctx, datastore.ReadOnly)
		if err != nil {
			return nil, err
		}
		if _, err := tx.Put(account, props("balance", int64(5))); err != nil {
			return nil, err
		}
		_, err = tx.Commit()
		return "ok", err
	})
	bank := datastore.NameKey("Bank", "b1", nil)
	for i := int64(1); i <= 3; i++ {
		if _, err := client.Put(ctx, datastore.IDKey("Acct", i, bank), props("n", i)); err != nil {
			panic(err)
		}
	}
	step("an ancestor query in a transaction", func() (any, error) {
		tx, err := client.NewTransaction(ctx)
		if err != nil {
			return nil, err
		}
		defer tx.Rollback()
		var pls []datastore.PropertyList
		if _, err := client.GetAll(ctx, datastore.NewQuery("Acct").Ancestor(bank).Transaction(tx), &pls); err != nil {
			return nil, err
		}
		ns := []any{}
		for _, pl := range pls {
			ns = append(ns, pl[0].Value)
		}
		return ns, nil
	})
	step("a count in a transaction", func() (any, error) {
		tx, err := client.NewTransaction(ctx)
		if err != nil {
			return nil, err
		}
		defer tx.Rollback()
		res, err := client.RunAggregationQuery(ctx, datastore.NewQuery("Acct").Ancestor(bank).Transaction(tx).NewAggregationQuery().WithCount("n"))
		if err != nil {
			return nil, err
		}
		return aggregated(res["n"]), nil
	})
	step("a missing entity in a transaction", func() (any, error) {
		tx, err := client.NewTransaction(ctx)
		if err != nil {
			return nil, err
		}
		defer tx.Rollback()
		var pl datastore.PropertyList
		err = tx.Get(datastore.NameKey("Account", "nobody", nil), &pl)
		if errors.Is(err, datastore.ErrNoSuchEntity) {
			return nil, nil
		}
		return "found", err
	})
	step("read in a rolled-back transaction", func() (any, error) {
		tx, err := client.NewTransaction(ctx)
		if err != nil {
			return nil, err
		}
		if err := tx.Rollback(); err != nil {
			return nil, err
		}
		var pl datastore.PropertyList
		return "ok", tx.Get(account, &pl)
	})
	step("commit twice", func() (any, error) {
		tx, err := client.NewTransaction(ctx)
		if err != nil {
			return nil, err
		}
		if _, err := tx.Put(account, props("balance", int64(50))); err != nil {
			return nil, err
		}
		if _, err := tx.Commit(); err != nil {
			return nil, err
		}
		_, err = tx.Commit()
		return "ok", err
	})
	step("after it, the balance", balance)
	step("an incomplete key in a transaction", func() (any, error) {
		var pending *datastore.PendingKey
		commit, err := client.RunInTransaction(ctx, func(tx *datastore.Transaction) error {
			p, err := tx.Put(datastore.IncompleteKey("Account", nil), props("balance", int64(1)))
			pending = p
			return err
		})
		if err != nil {
			return nil, err
		}
		k := commit.Key(pending)
		var pl datastore.PropertyList
		if err := client.Get(ctx, k, &pl); err != nil {
			return nil, err
		}
		return obj{"complete": !k.Incomplete(), "read": pl[0].Value}, nil
	})
}
