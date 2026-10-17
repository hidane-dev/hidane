# Vendored protobuf definitions

The `.proto` files under `google/` are copied from
[googleapis/googleapis](https://github.com/googleapis/googleapis) at commit
`bb5b791bc013ef90cea669954f172f6da2590c57`, with the one local change listed below. They are licensed under the Apache License 2.0;
see [`LICENSE-googleapis`](LICENSE-googleapis).

Only `google/firestore/v1/*.proto`, `google/datastore/v1/*.proto` (Datastore mode, #81) and
their transitive imports are vendored. The
`google/protobuf/*` well-known types are supplied by the pure-Rust compiler
([protox](https://crates.io/crates/protox)), so no `protoc` is needed to build hidane.

## Local changes

- `google/firestore/v1/write.proto`: `string verify = 5;` in `Write.operation`, marked
  "hidane patch". The firebase-js-sdk and the mobile SDKs send it in transactions (their own
  copies of the file define it) and the official emulator accepts it, but googleapis does not
  publish it. Without the field, prost would drop it and the write would arrive with no
  operation.

- `google/datastore/v1/datastore.proto`: `int64 commit_version = 6;` in `CommitResponse`,
  marked "hidane patch" (#81). The official emulator's REST answer to a transactional commit in
  Datastore mode has a `commitVersion` (the commit time in microseconds) between
  `mutationResults` (3) and `commitTime` (8) that googleapis does not publish. The field number
  is hidane's, an unused one between those two, so that JSON prints the fields in the same
  order; gRPC clients see an unknown field.

To update: check out googleapis at the new commit, copy the same files, reapply the local
changes above, update the commit hash, and run `cargo test --workspace`.
