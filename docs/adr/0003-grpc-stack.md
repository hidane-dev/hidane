# ADR 0003: gRPC stack and sharing one port with REST / WebChannel

- **Status**: Accepted (option A, validated by the spike in #15)
- **Date**: 2026-10-09

## Context

The official emulator accepts HTTP/1.1 REST (JSON), plaintext HTTP/2 gRPC (h2c) and WebChannel on
**one port** and routes each connection by whether it is HTTP/2 (startup log:
`Detected HTTP/2 connection.` / `Detected non-HTTP/2 connection.`). Internally it runs a gRPC
server and a WebChannel server on ephemeral ports and forwards TCP to them (observed with `lsof`).

hidane must satisfy:

| Requirement | Source |
|---|---|
| h2c prior-knowledge gRPC (grpc-js, grpc-go, grpc-okhttp and grpc++ all use plaintext HTTP/2 against the emulator) | SDK sources, see [compatibility.md](../compatibility.md) |
| HTTP/1.1 REST and WebChannel (POST / GET, chunked responses) on the same port | Observed on v1.22.0 |
| gRPC reflection (`grpc.reflection.v1alpha`); the official one lists services but `grpcurl describe` fails on `google.api.api_visibility` | `results/official-v1.22.0-baseline.md` |
| Both `google.firestore.v1.Firestore` (17 RPCs) and the identical `v1beta1` service | Observed reflection output |
| 17 MB max message (SDK limits), keepalive that avoids the iOS 90 s GOAWAY (firebase-tools #11238) | SDK sources; issue |
| REST derived by gRPC transcoding (`google.api.http`); accept `Content-Type: text/plain` JSON from js-sdk | `firestore.proto` HTTP annotations; js-sdk `rest_connection.ts` |

Prior art in Rust: skunkteam/rust-firestore-emulator uses tonic 0.14 + axum 0.8.

## Options

### A. tonic + axum on one hyper server

- Mount tonic's `Routes` and an axum `Router` on the same hyper service; route by `content-type: application/grpc` / HTTP version. WebChannel is an axum handler
- Pro: one port, one process, same shape as the official emulator; large ecosystem (tower middleware, tonic-reflection)
- Con: tonic and axum must agree on the hyper 1.x version; h2c auto-detection relies on hyper's `auto` builder
- Open: can axum's streaming response flush chunked WebChannel back-channel data at the granularity the official emulator uses

### B. tonic only, hand-written HTTP/1.1

- Pro: fewer dependencies
- Con: REST transcoding and WebChannel become a lot of hand-written code

### C. Internal ports + TCP forwarding (like the official emulator)

- Pro: independent servers
- Con: more ports to reconcile with firebase-tools' `reservedPorts`; extra hop

### Proto code generation

- Build-time `prost-build` from vendored googleapis protos vs committed generated code vs a `googleapis-tonic`-style crate
- `v1beta1` needs a second generation pass; the `FileDescriptorSet` doubles as the reflection source

## Decision

**Option A**: tonic and axum behind one hyper-util server on one port. Protos are vendored under
`proto/` and compiled with protox (pure Rust, no `protoc`) plus `tonic-prost-build`; the encoded
`FileDescriptorSet` is embedded and served through reflection. REST routing will be driven by the
`google.api.http` annotations rather than hand-written handlers.

## Spike findings (#15)

Code: `crates/hidane/src/lib.rs`, tests: `crates/hidane/tests/single_port.rs`.

1. **One port works.** `hyper_util::server::conn::auto::Builder` reads the connection preface and
   serves HTTP/1.1 and h2c on the same listener. Verified with `grpcurl` (list, describe, invoke),
   `curl` over HTTP/1.1 and `curl --http2-prior-knowledge`, and with a tonic client in the tests.
2. **Dispatch by content type, not by path.** tonic registers a `/<package.Service>/*` wildcard
   per service, and the WebChannel URLs `/google.firestore.v1.Firestore/{Listen,Write}/channel`
   fall under that prefix. hidane therefore sends `content-type: application/grpc*` to tonic and
   everything else to the HTTP router. The official emulator splits by HTTP version instead
   (HTTP/2 → gRPC). The only observable difference is that a non-gRPC request over h2c reaches
   the HTTP router in hidane; no SDK sends one.
3. **Own accept loop, not `axum::serve` or `tonic::transport::Server`.** It is the place to set
   HTTP/2 keepalive and frame limits (#25) and graceful shutdown on SIGINT (#12). `TCP_NODELAY`
   is set on every accepted socket.
4. **Chunked responses flush per chunk.** A response that sends one chunk, waits 600 ms and sends
   another delivers the first chunk to an HTTP/1.1 client well within 400 ms, which is what a
   WebChannel back channel needs (ADR 0004).
5. **Codegen.** `generate_default_stubs(true)` lets the service start as an empty
   `impl Firestore for FirestoreService {}`: all 17 RPCs, streaming ones included, answer
   `UNIMPLEMENTED` until implemented. Generated comments are not doctests (`doctest = false`).
6. **Reflection.** v1 and v1alpha are both served. Because every imported googleapis file is in
   the descriptor set, `grpcurl describe` works, unlike the official emulator.
7. **Footprint of the skeleton** (release build, macOS arm64, measured while other jobs were
   running, load average about 20): binary 4.4 MiB, 5.1 ms mean from exec to the first accepted
   connection (n = 10), 6.8 MiB RSS. Indicative only; the real numbers come from the benchmark
   harness (#47, #48).

Versions at the time of the spike: tonic 0.14.6, axum 0.8.9, hyper 1.12.0, hyper-util 0.1.21,
prost 0.14.4, protox 0.10.0.

## Consequences

- WebChannel (ADR 0004) is an axum handler on the HTTP side of the dispatcher
- `google.firestore.v1beta1.Firestore` still needs a second service registration (#25)
- keepalive / max message size / reflection settings are checked against the official emulator in the parity suite
