# Vendored protobuf definitions

The `.proto` files under `google/` are copied unmodified from
[googleapis/googleapis](https://github.com/googleapis/googleapis) at commit
`bb5b791bc013ef90cea669954f172f6da2590c57`. They are licensed under the Apache License 2.0;
see [`LICENSE-googleapis`](LICENSE-googleapis).

Only `google/firestore/v1/*.proto` and their transitive imports are vendored. The
`google/protobuf/*` well-known types are supplied by the pure-Rust compiler
([protox](https://crates.io/crates/protox)), so no `protoc` is needed to build hidane.

To update: check out googleapis at the new commit, copy the same files, update the commit hash
above, and run `cargo test --workspace`.
