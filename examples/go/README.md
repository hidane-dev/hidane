# go

[cloud.google.com/go/firestore](https://pkg.go.dev/cloud.google.com/go/firestore) with `go test`,
against hidane started on its own. [`firestore_test.go`](firestore_test.go) writes and reads a
document, queries, orders and counts, runs a transaction and listens to a query, each test on an
empty database.

## Run it

With Go 1.26 or newer and `hidane` on `PATH` (any channel in the
[README](../../README.md#install)):

```sh
hidane --host 127.0.0.1 --port 8080 &
FIRESTORE_EMULATOR_HOST=127.0.0.1:8080 go test ./...
curl -X POST http://127.0.0.1:8080/shutdown   # stops hidane
```

With `FIRESTORE_EMULATOR_HOST` set, the client connects to the emulator at that address without
credentials; the tests fail with a message when it is not set. `demo-hidane` is a demo project ID,
which needs no Google Cloud project.

## In CI

[`examples.yml`](../../.github/workflows/examples.yml) starts hidane the same way and runs
`go test ./...` with the Go version in `go.mod`, with hidane built from the pull request and with
the published v0.4.0.
