# ADR 0003: gRPC stack and sharing one port with REST / WebChannel

- **Status**: Draft (undecided)
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
| gRPC reflection (`grpc.reflection.v1alpha`); the official one lists services but `grpcurl describe` fails on `google.api.api_visibility` | `results/latency-grpc-reflection-vs-proto-official-v1.22.0.txt` |
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

Undecided. Placeholder: **A (tonic + axum, single hyper server)**. Vendor the protos, generate with
`prost-build`, reuse the `FileDescriptorSet` for reflection, and drive REST routing from the
`google.api.http` annotations rather than hand-written handlers.

## Consequences

- The "single-port multiplexing" issue is the spike that validates this ADR
- WebChannel (ADR 0004) is added as a handler on A; the v0.1 core is designed with that in mind
- keepalive / max message size / reflection settings are checked against the official emulator in the parity suite
